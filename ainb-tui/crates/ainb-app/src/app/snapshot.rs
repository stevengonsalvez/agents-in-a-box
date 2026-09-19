// ABOUTME: Periodic session snapshot system for disaster recovery
// Takes snapshots of all known sessions every 30 minutes, capturing
// tmux state, git status, and pane content. Enables recovery after tmux crashes.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::process::Command;

use crate::interactive::session_manager::SessionStore;

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub timestamp: DateTime<Utc>,
    pub sessions: Vec<SessionSnapshotEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionSnapshotEntry {
    pub tmux_session_name: String,
    pub session_id: String,
    pub worktree_path: PathBuf,
    pub workspace_name: String,
    pub agent_type: String,
    pub git_branch: Option<String>,
    pub git_dirty_files: Vec<String>,
    pub pane_content: Option<String>,
    pub tmux_alive: bool,
}

pub struct SnapshotManager;

impl SnapshotManager {
    pub async fn take_snapshot() -> Result<SessionSnapshot> {
        let store = crate::cli::util::load_session_store_async().await?;
        let mut entries = Vec::new();

        for (tmux_name, metadata) in store.sessions() {
            let tmux_alive = Self::check_tmux_alive(tmux_name).await;
            let git_branch = Self::get_git_branch(&metadata.worktree_path).await;
            let git_dirty_files = Self::get_git_dirty_files(&metadata.worktree_path).await;
            let pane_content = if tmux_alive {
                Self::capture_pane(tmux_name).await.map(|pane| Self::kept_pane(&pane))
            } else {
                None
            };

            entries.push(SessionSnapshotEntry {
                tmux_session_name: tmux_name.clone(),
                session_id: metadata.session_id.to_string(),
                worktree_path: metadata.worktree_path.clone(),
                workspace_name: metadata.workspace_name.clone(),
                agent_type: format!("{:?}", metadata.agent_type),
                git_branch,
                git_dirty_files,
                pane_content,
                tmux_alive,
            });
        }

        Ok(SessionSnapshot {
            timestamp: Utc::now(),
            sessions: entries,
        })
    }

    /// A pane capture as a snapshot keeps it. Written to
    /// ~/.agents-in-a-box/snapshots and kept, so scrubbed first, as every other
    /// place pane text is kept is.
    fn kept_pane(pane: &str) -> String {
        crate::fleet::bridge::redact::scrub(pane)
    }

    pub async fn save_snapshot(snapshot: &SessionSnapshot) -> Result<PathBuf> {
        let base = dirs::home_dir().unwrap_or_default().join(".agents-in-a-box").join("snapshots");
        Self::save_snapshot_in(&base, snapshot).await
    }

    async fn save_snapshot_in(
        base: &std::path::Path,
        snapshot: &SessionSnapshot,
    ) -> Result<PathBuf> {
        let dirname = snapshot.timestamp.format("%Y-%m-%d-%H%M%S").to_string();
        let dir = base.join(&dirname);
        tokio::fs::create_dir_all(&dir).await?;

        // Write snapshot.json
        let content = serde_json::to_string_pretty(snapshot)?;
        tokio::fs::write(dir.join("snapshot.json"), content).await?;

        // Copy sessions.json verbatim
        let sessions_path = SessionStore::storage_path();
        if sessions_path.exists() {
            tokio::fs::copy(&sessions_path, dir.join("sessions.json")).await?;
        }

        // Write individual pane captures
        for entry in &snapshot.sessions {
            if let Some(ref content) = entry.pane_content {
                let fname = format!("{}.pane.txt", entry.tmux_session_name);
                tokio::fs::write(dir.join(fname), content).await?;
            }
        }

        tracing::info!("Session snapshot saved to {:?}", dir);
        Ok(dir)
    }

    pub async fn prune_snapshots(keep: usize) -> Result<usize> {
        let base = dirs::home_dir().unwrap_or_default().join(".agents-in-a-box").join("snapshots");
        if !base.exists() {
            return Ok(0);
        }

        let mut entries: Vec<_> = std::fs::read_dir(&base)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();

        // Sort by name (chronological since names are timestamp-based)
        entries.sort_by_key(|e| e.file_name());

        let to_remove = if entries.len() > keep {
            entries.len() - keep
        } else {
            return Ok(0);
        };

        let mut removed = 0;
        for entry in entries.into_iter().take(to_remove) {
            if std::fs::remove_dir_all(entry.path()).is_ok() {
                removed += 1;
            }
        }

        if removed > 0 {
            tracing::info!("Pruned {} old snapshots (keeping {})", removed, keep);
        }
        Ok(removed)
    }

    async fn check_tmux_alive(session_name: &str) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", &format!("={session_name}")])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn get_git_branch(worktree_path: &PathBuf) -> Option<String> {
        Command::new("git")
            .args([
                "-C",
                &worktree_path.to_string_lossy(),
                "branch",
                "--show-current",
            ])
            .output()
            .await
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
    }

    async fn get_git_dirty_files(worktree_path: &PathBuf) -> Vec<String> {
        Command::new("git")
            .args([
                "-C",
                &worktree_path.to_string_lossy(),
                "diff",
                "--name-only",
            ])
            .output()
            .await
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn capture_pane(session_name: &str) -> Option<String> {
        Command::new("tmux")
            .args(["capture-pane", "-t", session_name, "-p"])
            .output()
            .await
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).to_string())
                } else {
                    None
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_token_in_a_captured_pane_is_redacted_in_the_snapshot_files() {
        let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let base = tempfile::tempdir().expect("scratch snapshots dir");
        let snapshot = SessionSnapshot {
            timestamp: Utc::now(),
            sessions: vec![SessionSnapshotEntry {
                tmux_session_name: "tmux_seeded".to_string(),
                session_id: "s1".to_string(),
                worktree_path: PathBuf::from("/w/seeded"),
                workspace_name: "seeded".to_string(),
                agent_type: "Claude".to_string(),
                git_branch: None,
                git_dirty_files: Vec::new(),
                pane_content: Some(SnapshotManager::kept_pane(&format!(
                    "$ export GITHUB_TOKEN={token}\n"
                ))),
                tmux_alive: true,
            }],
        };

        let dir = SnapshotManager::save_snapshot_in(base.path(), &snapshot).await.expect("saved");

        for file in ["snapshot.json", "tmux_seeded.pane.txt"] {
            let written = std::fs::read_to_string(dir.join(file)).expect(file);
            assert!(
                !written.contains(token),
                "{file} kept the token:\n{written}"
            );
            assert!(
                written.contains(crate::fleet::bridge::redact::REDACTED),
                "{file} names the redaction:\n{written}"
            );
        }
    }
}
