// ABOUTME: Session loader that queries Docker containers and worktrees to load active sessions
// Groups sessions by their source repository for display

#![allow(dead_code)]

use crate::config::AppConfig;
use crate::docker::ContainerManager;
use crate::git::WorktreeManager;
use crate::models::{Session, SessionMode, SessionStatus, Workspace};
use crate::tmux::TmuxSession;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};
use uuid::Uuid;

pub struct SessionLoader {
    container_manager: ContainerManager,
    worktree_manager: WorktreeManager,
    config: AppConfig,
}

impl SessionLoader {
    pub async fn new() -> Result<Self> {
        let container_manager = ContainerManager::new().await?;
        let worktree_manager = WorktreeManager::new()?;
        let config = AppConfig::load()?;

        Ok(Self {
            container_manager,
            worktree_manager,
            config,
        })
    }

    /// Load all active sessions from Docker containers and worktrees
    pub async fn load_active_sessions(&self) -> Result<Vec<Workspace>> {
        info!("Loading active sessions from Docker containers");

        // Get all Claude-managed containers
        let containers = self.container_manager.list_agents_containers().await?;
        info!("Found {} Claude-managed containers", containers.len());

        // Group sessions by their source repository
        let mut workspace_map: HashMap<PathBuf, Workspace> = HashMap::new();

        for container in containers {
            // Extract session ID from container labels
            let session_id = container
                .labels
                .as_ref()
                .and_then(|labels| labels.get("agents-session-id"))
                .and_then(|id| Uuid::parse_str(id).ok());

            if let Some(session_id) = session_id {
                debug!("Processing container for session {}", session_id);

                // Get worktree information for this session
                match self.worktree_manager.get_worktree_info(session_id) {
                    Ok(worktree_info) => {
                        // Create session from container and worktree info
                        let mut session = Session::new(
                            worktree_info.branch_name.clone(),
                            worktree_info.path.to_string_lossy().to_string(), // Use worktree path, not source repo
                        );
                        session.id = session_id;
                        session.container_id = container.id;
                        session.branch_name = worktree_info.branch_name.clone();
                        session.mode = SessionMode::Boss;

                        // Set session status based on container state
                        let state = container.state.as_deref().unwrap_or("unknown");
                        session.set_status(match state {
                            "running" => SessionStatus::Running,
                            "paused" => SessionStatus::Stopped,
                            "exited" | "dead" => SessionStatus::Stopped,
                            _ => {
                                SessionStatus::Error(format!("Unknown container state: {}", state))
                            }
                        });

                        let workspace_name =
                            crate::interactive::InteractiveSessionManager::derive_workspace_name(
                                &worktree_info.path,
                                &worktree_info.source_repository,
                            );

                        // Add session to appropriate workspace
                        let workspace = workspace_map
                            .entry(worktree_info.source_repository.clone())
                            .or_insert_with(|| {
                                Workspace::new(
                                    workspace_name,
                                    worktree_info.source_repository.clone(),
                                )
                            });

                        workspace.add_session(session);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to get worktree info for session {}: {}",
                            session_id, e
                        );

                        // Container exists but worktree is missing - this is an orphaned container
                        // We have a few options:
                        // 1. Clean up the orphaned container
                        // 2. Create a session marked as having missing worktree
                        // 3. Ignore it

                        // For now, let's create a session marked as having issues
                        // so the user can see it and decide what to do

                        info!(
                            "Creating session entry for orphaned container {}",
                            session_id
                        );

                        // Create a session with error status indicating missing worktree
                        let mut session = Session::new(
                            format!(
                                "orphaned-{}",
                                session_id.to_string().split('-').next().unwrap_or("session")
                            ),
                            format!("Missing worktree for session {}", session_id),
                        );
                        session.id = session_id;
                        session.container_id = container.id.clone();
                        session.set_status(SessionStatus::Error(
                            "Worktree missing - container orphaned".to_string(),
                        ));
                        session.mode = SessionMode::Boss;

                        // Try to determine the original workspace from container labels or name
                        let workspace_name = container
                            .names
                            .as_ref()
                            .and_then(|names| names.first())
                            .and_then(|name| name.strip_prefix('/'))
                            .and_then(|name| name.split('-').next())
                            .unwrap_or("unknown")
                            .to_string();

                        // Create or find workspace for orphaned session
                        let workspace = workspace_map
                            .entry(std::path::PathBuf::from(format!(
                                "/unknown/{}",
                                workspace_name
                            )))
                            .or_insert_with(|| {
                                // Create a placeholder workspace for orphaned sessions
                                Workspace::new(
                                    workspace_name.clone(),
                                    std::path::PathBuf::from(format!(
                                        "/unknown/{}",
                                        workspace_name
                                    )),
                                )
                            });

                        workspace.add_session(session);

                        info!(
                            "Added orphaned session {} to workspace {}",
                            session_id, workspace_name
                        );
                    }
                }
            } else {
                warn!(
                    "Container {} has no session ID label",
                    container.id.unwrap_or_default()
                );
            }
        }

        // Also check for worktrees without containers (orphaned worktrees)
        match self.worktree_manager.list_all_worktrees() {
            Ok(worktree_list) => {
                for (session_id, worktree_info) in worktree_list {
                    // Check if we already processed this session from containers
                    let already_processed = workspace_map
                        .values()
                        .any(|w| w.sessions.iter().any(|s| s.id == session_id));

                    if !already_processed {
                        debug!("Found orphaned worktree for session {}", session_id);

                        // Skip Interactive (tmux-managed) sessions - those are loaded separately
                        let tmux_probe = TmuxSession::new(
                            worktree_info.branch_name.clone(),
                            "claude".to_string(),
                        );
                        if tmux_probe.does_session_exist().await {
                            info!(
                                "Skipping tmux-managed Interactive session {} ({}) in Boss mode loader",
                                session_id,
                                tmux_probe.name()
                            );
                            continue;
                        }

                        // Worktree exists but has no container AND no tmux session
                        // This is a truly orphaned worktree (e.g., from a closed Interactive session)
                        // Don't create a Boss session for it - just log and skip
                        // It can be cleaned up via the cleanup function
                        info!(
                            "Skipping orphaned worktree {} (no container, no tmux) - candidate for cleanup",
                            session_id
                        );
                    }
                }
            }
            Err(e) => {
                warn!("Failed to list worktrees: {}", e);
            }
        }

        // Convert map to sorted vector
        let mut workspaces: Vec<Workspace> = workspace_map.into_values().collect();
        workspaces.sort_by(|a, b| a.name.cmp(&b.name));

        info!(
            "Loaded {} workspaces with active sessions",
            workspaces.len()
        );
        Ok(workspaces)
    }

    /// Load sessions from persistence (e.g., ~/.agents-box/sessions.json)
    pub async fn load_from_persistence(&self) -> Result<Vec<Session>> {
        // TODO: Implement loading from ~/.agents-box/sessions.json
        // For now, return empty vec
        Ok(vec![])
    }

    /// Create a new session browser to select repository for new session
    pub async fn get_available_repositories(&self) -> Result<Vec<PathBuf>> {
        // Use workspace scanner to find repositories
        use crate::git::WorkspaceScanner;

        let scanner = WorkspaceScanner::with_additional_paths(
            self.config.workspace_defaults.workspace_scan_paths.clone(),
        )
        .with_exclude_paths(self.config.workspace_defaults.exclude_paths.clone());
        let scan_result = scanner.scan()?;

        let max_repos = self.config.workspace_defaults.max_repositories;
        let total_found = scan_result.workspaces.len();

        let repos: Vec<PathBuf> =
            scan_result.workspaces.into_iter().map(|w| w.path).take(max_repos).collect();

        info!(
            "Found {} repositories (showing {} of {}, limit: {})",
            total_found,
            repos.len(),
            total_found,
            max_repos
        );
        Ok(repos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_session_loader_creation() {
        let loader = SessionLoader::new().await;
        assert!(loader.is_ok());
    }
}
