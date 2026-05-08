// ABOUTME: Session lifecycle management that coordinates worktrees and Docker containers

#![allow(dead_code)]

use super::{
    AgentsDevConfig, AgentsDevProgress, ContainerConfig, ContainerManager, ContainerStatus,
    SessionContainer, SessionProgress,
};
use crate::config::{
    AppConfig, ContainerTemplate, McpInitializer, ProjectConfig, apply_mcp_init_result,
};
use crate::credentials;
use crate::git::{WorktreeInfo, WorktreeManager};
use crate::models::{ClaudeModel, Session, SessionAgentType, SessionStatus};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum SessionLifecycleError {
    #[error("Worktree error: {0}")]
    Worktree(#[from] crate::git::WorktreeError),
    #[error("Container error: {0}")]
    Container(#[from] super::ContainerError),
    #[error("Session not found: {0}")]
    SessionNotFound(Uuid),
    #[error("Session already exists: {0}")]
    SessionAlreadyExists(Uuid),
    #[error("Invalid session state: {0}")]
    InvalidState(String),
    #[error("Configuration error: {0}")]
    ConfigError(String),
}

pub struct SessionLifecycleManager {
    worktree_manager: WorktreeManager,
    container_manager: ContainerManager,
    active_sessions: HashMap<Uuid, SessionState>,
    app_config: AppConfig,
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub session: Session,
    pub worktree_info: Option<WorktreeInfo>,
    pub container: Option<SessionContainer>,
}

#[derive(Debug, Clone)]
pub struct SessionRequest {
    pub session_id: Uuid,
    pub workspace_name: String,
    pub workspace_path: PathBuf,
    pub branch_name: String,
    pub base_branch: Option<String>,
    pub container_config: Option<ContainerConfig>,
    pub skip_permissions: bool,
    pub mode: crate::models::SessionMode,
    pub boss_prompt: Option<String>,
    pub agent_type: SessionAgentType,
    pub model: Option<ClaudeModel>,
}

impl SessionLifecycleManager {
    pub async fn new() -> Result<Self, SessionLifecycleError> {
        let worktree_manager = WorktreeManager::new().map_err(|e| {
            SessionLifecycleError::ConfigError(format!("Failed to create worktree manager: {}", e))
        })?;

        let container_manager = ContainerManager::new().await?;

        let app_config = AppConfig::load().map_err(|e| {
            SessionLifecycleError::ConfigError(format!("Failed to load config: {}", e))
        })?;

        Ok(Self {
            worktree_manager,
            container_manager,
            active_sessions: HashMap::new(),
            app_config,
        })
    }

    /// Create a new development session with isolated worktree and container
    ///
    /// **DEPRECATED**: Use `create_session()` instead for unified session creation across all container templates.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_session() instead for unified session creation"
    )]
    pub async fn create_session_legacy(
        &mut self,
        request: SessionRequest,
    ) -> Result<SessionState, SessionLifecycleError> {
        tracing::warn!(
            "create_session_legacy is deprecated. Use create_session() for unified session creation."
        );
        self.create_session_with_logs(request, None).await
    }

    /// Create a new agents-dev session using the native agents_dev module
    ///
    /// **DEPRECATED**: Use `create_session()` instead for unified session creation across all container templates.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_session() instead for unified session creation"
    )]
    pub async fn create_agents_dev_session(
        &mut self,
        request: SessionRequest,
    ) -> Result<SessionState, SessionLifecycleError> {
        tracing::warn!(
            "create_agents_dev_session is deprecated. Use create_session() for unified session creation."
        );
        self.create_session(request, None).await
    }

    /// Create a new agents-dev session using the native agents_dev module with progress tracking
    ///
    /// **DEPRECATED**: Use `create_session()` with SessionProgress instead for unified session creation.
    #[deprecated(
        since = "0.1.0",
        note = "Use create_session() with SessionProgress instead for unified session creation"
    )]
    pub async fn create_agents_dev_session_with_logs(
        &mut self,
        request: SessionRequest,
        progress_sender: Option<mpsc::Sender<AgentsDevProgress>>,
    ) -> Result<SessionState, SessionLifecycleError> {
        info!(
            "Creating new agents-dev session {} for workspace {}",
            request.session_id, request.workspace_name
        );

        // Check if session already exists
        if self.active_sessions.contains_key(&request.session_id) {
            return Err(SessionLifecycleError::SessionAlreadyExists(
                request.session_id,
            ));
        }

        // Load project configuration to check mount_claude_config setting
        let project_config =
            ProjectConfig::load_from_dir(&request.workspace_path).map_err(|e| {
                SessionLifecycleError::ConfigError(format!("Failed to load project config: {}", e))
            })?;

        let mount_claude_config = project_config.as_ref().map_or(true, |pc| pc.mount_claude_config);
        info!(
            "Project mount_claude_config setting: {}",
            mount_claude_config
        );

        // Create worktree
        let worktree_info = self.worktree_manager.create_worktree(
            request.session_id,
            &request.workspace_path,
            &request.branch_name,
            request.base_branch.as_deref(),
        )?;

        info!("Created worktree at: {}", worktree_info.path.display());

        // Create session model
        let mut session = Session::new_with_options(
            format!("{}-{}", request.workspace_name, request.branch_name),
            worktree_info.path.to_string_lossy().to_string(), // Use worktree path, not original repo path
            request.skip_permissions,
            request.mode.clone(),
            request.boss_prompt.clone(),
            request.agent_type,
            request.model,
        );
        session.id = request.session_id;
        session.branch_name = request.branch_name.clone();

        // Use agents_dev module to create container
        let agents_dev_config = AgentsDevConfig {
            image_name: "agents-box:agents-dev".to_string(),
            memory_limit: None,
            gpu_access: None,
            force_rebuild: false,
            no_cache: false,
            continue_session: false,
            skip_permissions: request.skip_permissions,
            env_vars: std::collections::HashMap::new(),
        };

        // Create the agents-dev container using the native module
        let container_id = match super::create_agents_dev_session(
            &worktree_info.path,
            agents_dev_config,
            request.session_id,
            progress_sender,
            mount_claude_config,
        )
        .await
        {
            Ok(id) => {
                info!("Created agents-dev container: {}", id);
                session.container_id = Some(id.clone());
                id
            }
            Err(e) => {
                // Clean up worktree if container creation fails
                if let Err(cleanup_err) = self.worktree_manager.remove_worktree(request.session_id)
                {
                    warn!(
                        "Failed to cleanup worktree after container creation failure: {}",
                        cleanup_err
                    );
                }
                return Err(SessionLifecycleError::ConfigError(format!(
                    "Failed to create agents-dev container: {}",
                    e
                )));
            }
        };

        // Verify container actually started by checking its status
        let container_status = self.container_manager.get_container_status(&container_id).await;
        match container_status {
            Ok(ContainerStatus::Running) => {
                session.set_status(SessionStatus::Running);
                info!("Container {} is running successfully", container_id);
            }
            Ok(status) => {
                warn!(
                    "Container {} is not running, status: {:?}",
                    container_id, status
                );
                session.set_status(match status {
                    ContainerStatus::Stopped | ContainerStatus::NotFound => SessionStatus::Stopped,
                    ContainerStatus::Error(msg) => SessionStatus::Error(msg),
                    _ => SessionStatus::Stopped,
                });
            }
            Err(e) => {
                error!(
                    "Failed to check container status for {}: {}",
                    container_id, e
                );
                session.set_status(SessionStatus::Error(format!(
                    "Failed to verify container status: {}",
                    e
                )));
            }
        }

        let session_state = SessionState {
            session,
            worktree_info: Some(worktree_info),
            container: None, // agents_dev module manages the container directly
        };

        self.active_sessions.insert(request.session_id, session_state.clone());

        info!(
            "Successfully created agents-dev session {}",
            request.session_id
        );
        Ok(session_state)
    }

    /// Create a new development session with isolated worktree and container with optional log sender
    pub async fn create_session_with_logs(
        &mut self,
        request: SessionRequest,
        log_sender: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<SessionState, SessionLifecycleError> {
        info!(
            "Creating new session {} for workspace {} using adapter to unified path",
            request.session_id, request.workspace_name
        );

        // Create progress adapter that converts SessionProgress to String logs
        let (progress_tx, mut progress_rx) = mpsc::channel::<SessionProgress>(100);

        // Spawn task to convert progress updates to string logs
        if let Some(log_sender) = log_sender {
            tokio::spawn(async move {
                while let Some(progress) = progress_rx.recv().await {
                    let log_message = progress.description();
                    let _ = log_sender.send(log_message);

                    // Stop listening when complete
                    if progress.is_complete() {
                        break;
                    }
                }
            });
        }

        // Use the unified session creation method
        self.create_session(request, Some(progress_tx)).await
    }

    /// Start a session (start the container if it exists)
    /// Start a session (start the container if it exists)
    pub async fn start_session(&mut self, session_id: Uuid) -> Result<(), SessionLifecycleError> {
        info!("Starting session {}", session_id);

        let session_state = self
            .active_sessions
            .get_mut(&session_id)
            .ok_or(SessionLifecycleError::SessionNotFound(session_id))?;

        if let Some(ref mut container) = session_state.container {
            self.container_manager.start_container(container).await?;
            session_state.session.set_status(SessionStatus::Running);
            info!("Started container for session {}", session_id);
        } else {
            // No container, just mark as running
            session_state.session.set_status(SessionStatus::Running);
            info!("Session {} marked as running (no container)", session_id);
        }

        Ok(())
    }

    /// Stop a session (stop the container if it exists)
    pub async fn stop_session(&mut self, session_id: Uuid) -> Result<(), SessionLifecycleError> {
        info!("Stopping session {}", session_id);

        let session_state = self
            .active_sessions
            .get_mut(&session_id)
            .ok_or(SessionLifecycleError::SessionNotFound(session_id))?;

        if let Some(ref mut container) = session_state.container {
            self.container_manager.stop_container(container).await?;
            session_state.session.set_status(SessionStatus::Stopped);
            info!("Stopped container for session {}", session_id);
        } else {
            session_state.session.set_status(SessionStatus::Stopped);
            info!("Session {} marked as stopped (no container)", session_id);
        }

        Ok(())
    }

    /// Remove a session (cleanup worktree and container)
    pub async fn remove_session(&mut self, session_id: Uuid) -> Result<(), SessionLifecycleError> {
        info!("Removing session {}", session_id);

        let mut session_state = self
            .active_sessions
            .remove(&session_id)
            .ok_or(SessionLifecycleError::SessionNotFound(session_id))?;

        // Stop and remove container if it exists
        if let Some(ref mut container) = session_state.container {
            if container.is_running() {
                self.container_manager.stop_container(container).await?;
            }
            self.container_manager.remove_container(container).await?;
            info!("Removed container for session {}", session_id);
        }

        // Remove worktree
        if session_state.worktree_info.is_some() {
            self.worktree_manager.remove_worktree(session_id)?;
            info!("Removed worktree for session {}", session_id);
        }

        info!("Successfully removed session {}", session_id);
        Ok(())
    }

    /// Get session information
    pub fn get_session(&self, session_id: Uuid) -> Option<&SessionState> {
        self.active_sessions.get(&session_id)
    }

    /// List all active sessions
    pub fn list_sessions(&self) -> Vec<&SessionState> {
        self.active_sessions.values().collect()
    }

    /// Update session status by checking container status
    pub async fn refresh_session_status(
        &mut self,
        session_id: Uuid,
    ) -> Result<(), SessionLifecycleError> {
        let session_state = self
            .active_sessions
            .get_mut(&session_id)
            .ok_or(SessionLifecycleError::SessionNotFound(session_id))?;

        if let Some(ref mut container) = session_state.container {
            if let Some(ref container_id) = container.container_id {
                let status = self.container_manager.get_container_status(container_id).await?;
                container.status = status.clone();

                // Update session status based on container status
                session_state.session.set_status(match status {
                    ContainerStatus::Running => SessionStatus::Running,
                    ContainerStatus::Stopped | ContainerStatus::NotFound => SessionStatus::Stopped,
                    ContainerStatus::Error(msg) => SessionStatus::Error(msg),
                    _ => SessionStatus::Stopped,
                });
            }
        }

        Ok(())
    }

    /// Refresh all session statuses
    pub async fn refresh_all_sessions(&mut self) -> Result<(), SessionLifecycleError> {
        let session_ids: Vec<Uuid> = self.active_sessions.keys().copied().collect();

        for session_id in session_ids {
            if let Err(e) = self.refresh_session_status(session_id).await {
                warn!("Failed to refresh status for session {}: {}", session_id, e);
            }
        }

        Ok(())
    }

    /// Get container logs for a session
    pub async fn get_session_logs(
        &self,
        session_id: Uuid,
        lines: Option<i64>,
    ) -> Result<Vec<String>, SessionLifecycleError> {
        let session_state = self
            .active_sessions
            .get(&session_id)
            .ok_or(SessionLifecycleError::SessionNotFound(session_id))?;

        if let Some(ref container) = session_state.container {
            if let Some(ref container_id) = container.container_id {
                let logs = self.container_manager.get_container_logs(container_id, lines).await?;
                return Ok(logs);
            }
        }

        Ok(vec![
            "No container associated with this session".to_string(),
        ])
    }

    /// Get the workspace URL for a session
    pub fn get_session_workspace_url(&self, session_id: Uuid, port: u16) -> Option<String> {
        self.active_sessions
            .get(&session_id)?
            .container
            .as_ref()?
            .get_workspace_url(port)
    }

    /// Clean up orphaned sessions (sessions with missing containers or worktrees)
    pub async fn cleanup_orphaned_sessions(&mut self) -> Result<Vec<Uuid>, SessionLifecycleError> {
        let mut orphaned = Vec::new();
        let session_ids: Vec<Uuid> = self.active_sessions.keys().copied().collect();

        for session_id in session_ids {
            let mut is_orphaned = false;

            // Check if worktree still exists
            if let Err(_) = self.worktree_manager.get_worktree_info(session_id) {
                warn!("Session {} has missing worktree", session_id);
                is_orphaned = true;
            }

            // Check if container still exists (if it should)
            if let Some(session_state) = self.active_sessions.get(&session_id) {
                if let Some(ref container) = session_state.container {
                    if let Some(ref container_id) = container.container_id {
                        match self.container_manager.get_container_status(container_id).await {
                            Ok(ContainerStatus::NotFound) => {
                                warn!("Session {} has missing container", session_id);
                                is_orphaned = true;
                            }
                            Err(_) => {
                                warn!("Session {} container status check failed", session_id);
                                is_orphaned = true;
                            }
                            _ => {}
                        }
                    }
                }
            }

            if is_orphaned {
                orphaned.push(session_id);
                self.active_sessions.remove(&session_id);
            }
        }

        if !orphaned.is_empty() {
            info!("Cleaned up {} orphaned sessions", orphaned.len());
        }

        Ok(orphaned)
    }

    /// Apply project-specific configuration to container config
    fn apply_project_config(&self, config: &mut ContainerConfig, project_config: &ProjectConfig) {
        // Apply environment variables
        for (key, value) in &project_config.environment {
            config.environment_vars.insert(key.clone(), value.clone());
        }

        // Apply additional mounts
        for mount in &project_config.additional_mounts {
            *config = config.clone().with_volume(
                PathBuf::from(&mount.host_path),
                mount.container_path.clone(),
                mount.read_only,
            );
        }

        // Apply container config overrides if provided
        if let Some(template_config) = &project_config.container_config {
            if let Some(memory) = template_config.memory_limit {
                config.memory_limit = Some(memory * 1024 * 1024); // MB to bytes
            }

            if let Some(cpu) = template_config.cpu_limit {
                config.cpu_limit = Some(cpu);
            }

            // Add environment variables from template config
            for (key, value) in &template_config.environment {
                config.environment_vars.insert(key.clone(), value.clone());
            }
        }
    }

    /// Get available container templates
    pub fn get_container_templates(&self) -> &HashMap<String, ContainerTemplate> {
        &self.app_config.container_templates
    }

    /// Get app configuration
    pub fn get_app_config(&self) -> &AppConfig {
        &self.app_config
    }

    /// Unified session creation method that works with any container template
    ///
    /// This is the **recommended method** for creating new development sessions. It provides:
    /// - Support for all container templates (claude-dev, node, python, rust, etc.)
    /// - Unified progress tracking with SessionProgress enum
    /// - Consistent mounting and MCP server initialization
    /// - Project-specific configuration support
    ///
    /// This method replaces the deprecated claude-dev specific creation methods.
    pub async fn create_session(
        &mut self,
        request: SessionRequest,
        progress_sender: Option<mpsc::Sender<SessionProgress>>,
    ) -> Result<SessionState, SessionLifecycleError> {
        info!(
            "Creating new session {} for workspace {} using unified path",
            request.session_id, request.workspace_name
        );

        // Check if session already exists
        if self.active_sessions.contains_key(&request.session_id) {
            return Err(SessionLifecycleError::SessionAlreadyExists(
                request.session_id,
            ));
        }

        // Step 1: Load and validate configuration
        let (project_config, template) =
            self.load_session_configuration(&request, &progress_sender).await?;

        // Step 2: Create worktree
        let worktree_info = self.create_session_worktree(&request, &progress_sender).await?;

        // Step 3: Create base container configuration from template
        let mut container_config = self
            .create_base_container_config(&template, &worktree_info, &progress_sender)
            .await?;

        // Step 4: Apply project-specific overrides
        self.apply_project_overrides(
            &mut container_config,
            &project_config,
            &request,
            &progress_sender,
        )
        .await?;

        // Step 5: Initialize MCP servers
        let mcp_result = self
            .initialize_mcp_servers(
                &mut container_config,
                &request,
                &project_config,
                &progress_sender,
            )
            .await?;

        // Step 6: Apply mounting logic (unified for all templates)
        self.apply_mounting_logic(
            &mut container_config,
            &project_config,
            &mcp_result,
            &progress_sender,
        )
        .await?;

        // Step 7: Create and start container
        let container = self
            .create_and_start_container(request.session_id, container_config, &progress_sender)
            .await?;

        // Step 8: Create session model and register it
        let session_state = self.create_session_state(request, container, worktree_info).await?;

        // Send final progress update
        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::Ready).await;
        }

        info!(
            "Successfully created session {} using unified path",
            session_state.session.id
        );
        Ok(session_state)
    }

    /// Load and validate session configuration
    async fn load_session_configuration(
        &self,
        request: &SessionRequest,
        progress_sender: &Option<mpsc::Sender<SessionProgress>>,
    ) -> Result<(Option<ProjectConfig>, ContainerTemplate), SessionLifecycleError> {
        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::LoadingConfiguration).await;
        }

        // Load project configuration
        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::LoadingProjectConfig).await;
        }

        let project_config =
            ProjectConfig::load_from_dir(&request.workspace_path).map_err(|e| {
                SessionLifecycleError::ConfigError(format!("Failed to load project config: {}", e))
            })?;

        // Determine which template to use
        let template_name = project_config
            .as_ref()
            .and_then(|pc| pc.container_template.as_ref())
            .map(|s| s.as_str())
            .unwrap_or(&self.app_config.default_container_template);

        if let Some(ref tx) = progress_sender {
            let _ = tx
                .send(SessionProgress::ValidatingTemplate(
                    template_name.to_string(),
                ))
                .await;
        }

        let template = self
            .app_config
            .get_container_template(template_name)
            .ok_or_else(|| {
                SessionLifecycleError::ConfigError(format!(
                    "Container template '{}' not found",
                    template_name
                ))
            })?
            .clone();

        info!(
            "Using container template '{}' for session {}",
            template_name, request.session_id
        );
        Ok((project_config, template))
    }

    /// Create worktree for the session
    async fn create_session_worktree(
        &mut self,
        request: &SessionRequest,
        progress_sender: &Option<mpsc::Sender<SessionProgress>>,
    ) -> Result<WorktreeInfo, SessionLifecycleError> {
        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::CreatingWorktree).await;
        }

        let worktree_info = self.worktree_manager.create_worktree(
            request.session_id,
            &request.workspace_path,
            &request.branch_name,
            request.base_branch.as_deref(),
        )?;

        info!("Created worktree at: {}", worktree_info.path.display());

        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::InitializingWorkspace).await;
        }

        Ok(worktree_info)
    }

    /// Create base container configuration from template
    async fn create_base_container_config(
        &self,
        template: &ContainerTemplate,
        worktree_info: &WorktreeInfo,
        progress_sender: &Option<mpsc::Sender<SessionProgress>>,
    ) -> Result<ContainerConfig, SessionLifecycleError> {
        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::PreparingContainer).await;
        }

        let mut config = template.to_container_config();

        // Mount the worktree
        config = config.with_volume(
            worktree_info.path.clone(),
            "/workspace".to_string(),
            false, // read-write
        );

        Ok(config)
    }

    /// Apply project-specific configuration overrides
    async fn apply_project_overrides(
        &self,
        config: &mut ContainerConfig,
        project_config: &Option<ProjectConfig>,
        request: &SessionRequest,
        _progress_sender: &Option<mpsc::Sender<SessionProgress>>,
    ) -> Result<(), SessionLifecycleError> {
        if let Some(project_config) = project_config {
            self.apply_project_config(config, project_config);
        }

        // Set session mode environment variable
        let mode_str = match request.mode {
            crate::models::SessionMode::Interactive => "interactive",
            crate::models::SessionMode::Boss => "boss",
        };
        config
            .environment_vars
            .insert("AGENTS_BOX_MODE".to_string(), mode_str.to_string());
        info!(
            "Set session mode to '{}' for session {}",
            mode_str, request.session_id
        );

        // Set ANTHROPIC_API_KEY if configured in the system keychain
        // This allows pay-as-you-go API usage instead of Pro/Max subscription
        match credentials::get_anthropic_api_key() {
            Ok(Some(api_key)) => {
                config.environment_vars.insert(
                    "ANTHROPIC_API_KEY".to_string(),
                    api_key,
                );
                info!(
                    "Set ANTHROPIC_API_KEY from keychain for session {}",
                    request.session_id
                );
            }
            Ok(None) => {
                // No API key configured - will use claude auth (Pro/Max plan)
                info!(
                    "No ANTHROPIC_API_KEY configured, using claude auth for session {}",
                    request.session_id
                );
            }
            Err(e) => {
                warn!(
                    "Failed to retrieve ANTHROPIC_API_KEY from keychain: {} - using claude auth for session {}",
                    e, request.session_id
                );
            }
        }

        // Set boss prompt if in boss mode
        if let Some(ref prompt) = request.boss_prompt {
            config.environment_vars.insert("AGENTS_BOX_PROMPT".to_string(), prompt.clone());
            info!("Set boss prompt for session {}", request.session_id);
        }

        // Apply skip_permissions flag if requested
        if request.skip_permissions {
            let current_flag =
                config.environment_vars.get("CLAUDE_CONTINUE_FLAG").cloned().unwrap_or_default();
            let new_flag = if current_flag.is_empty() {
                "--dangerously-skip-permissions".to_string()
            } else {
                format!("{} --dangerously-skip-permissions", current_flag)
            };
            config.environment_vars.insert("CLAUDE_CONTINUE_FLAG".to_string(), new_flag);
            info!(
                "Added --dangerously-skip-permissions flag to session {}",
                request.session_id
            );

            // Update auth .claude.json to set hasTrustDialogAccepted=true to avoid bypass warning
            if let Err(e) = Self::update_auth_claude_json_for_skip_permissions() {
                warn!(
                    "Failed to update auth .claude.json for skip permissions: {}",
                    e
                );
            }
        }

        Ok(())
    }

    /// Update the auth .claude.json file to set hasTrustDialogAccepted=true when skip permissions is enabled
    fn update_auth_claude_json_for_skip_permissions() -> Result<(), Box<dyn std::error::Error>> {
        use std::fs;

        let home_dir = dirs::home_dir().ok_or("Could not find home directory")?;
        let auth_claude_json = home_dir.join(".agents-in-a-box/auth/.claude.json");

        if !auth_claude_json.exists() {
            return Err("Auth .claude.json file not found".into());
        }

        // Read current config
        let contents = fs::read_to_string(&auth_claude_json)?;
        let mut config: serde_json::Value = serde_json::from_str(&contents)?;

        // Set hasTrustDialogAccepted to true globally
        config["hasTrustDialogAccepted"] = serde_json::Value::Bool(true);

        // Write back to file
        let updated_contents = serde_json::to_string_pretty(&config)?;
        fs::write(&auth_claude_json, updated_contents)?;

        info!("Updated auth .claude.json to set hasTrustDialogAccepted=true for skip permissions");
        Ok(())
    }

    /// Initialize MCP servers
    async fn initialize_mcp_servers(
        &self,
        config: &mut ContainerConfig,
        request: &SessionRequest,
        _project_config: &Option<ProjectConfig>,
        progress_sender: &Option<mpsc::Sender<SessionProgress>>,
    ) -> Result<crate::config::McpInitResult, SessionLifecycleError> {
        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::InitializingMcpServers).await;
        }

        // Use default hybrid strategy and get MCP servers from config
        let mcp_strategy = crate::config::McpInitStrategy::default();
        let mcp_servers = if self.app_config.mcp_servers.is_empty() {
            crate::config::McpServerConfig::defaults()
        } else {
            self.app_config.mcp_servers.clone()
        };

        let mcp_initializer = McpInitializer::new(mcp_strategy, mcp_servers);
        let mcp_result = mcp_initializer
            .initialize_for_session(request.session_id, &request.workspace_path, config)
            .await
            .map_err(|e| {
                SessionLifecycleError::ConfigError(format!(
                    "Failed to initialize MCP servers: {}",
                    e
                ))
            })?;

        // Apply MCP configuration to container
        apply_mcp_init_result(config, &mcp_result);

        info!(
            "MCP initialization completed for session {}",
            request.session_id
        );
        Ok(mcp_result)
    }

    /// Apply mounting logic for authentication and configuration
    async fn apply_mounting_logic(
        &self,
        config: &mut ContainerConfig,
        project_config: &Option<ProjectConfig>,
        _mcp_result: &crate::config::McpInitResult,
        progress_sender: &Option<mpsc::Sender<SessionProgress>>,
    ) -> Result<(), SessionLifecycleError> {
        // Mount claude-in-a-box authentication if requested and not already handled by MCP init
        if project_config.as_ref().map_or(true, |pc| pc.mount_claude_config) {
            if let Some(home_dir) = dirs::home_dir() {
                // First, mount the user's entire .claude directory if it exists
                // This allows access to CLAUDE.md and any other files it references
                let user_claude_dir = home_dir.join(".claude");
                if user_claude_dir.exists() && user_claude_dir.is_dir() {
                    *config = config.clone().with_volume(
                        user_claude_dir,
                        "/home/claude-user/.claude".to_string(),
                        false, // read-write so user can edit their memory and other files
                    );
                    info!("Mounting user's entire .claude directory from ~/.claude");
                }

                // Then mount agents-in-a-box auth credentials on top
                // This will override any .credentials.json from the host .claude directory
                let credentials_path = home_dir.join(".agents-in-a-box/auth/.credentials.json");
                if credentials_path.exists() {
                    *config = config.clone().with_volume(
                        credentials_path.clone(),
                        "/home/claude-user/.claude/.credentials.json".to_string(),
                        true, // read-only for security
                    );
                    info!(
                        "Mounting agents-in-a-box auth credentials from ~/.agents-in-a-box/auth/.credentials.json"
                    );

                    // ALSO set OAuth token as environment variable for redundancy
                    if let Ok(creds_content) = std::fs::read_to_string(&credentials_path) {
                        if let Ok(creds_json) =
                            serde_json::from_str::<serde_json::Value>(&creds_content)
                        {
                            if let Some(access_token) = creds_json
                                .get("claudeAiOauth")
                                .and_then(|oauth| oauth.get("accessToken"))
                                .and_then(|token| token.as_str())
                            {
                                info!(
                                    "Found OAuth access token in credentials, setting CLAUDE_CODE_OAUTH_TOKEN environment variable"
                                );
                                config.environment_vars.insert(
                                    "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                                    access_token.to_string(),
                                );
                            }
                        }
                    }
                } else {
                    warn!(
                        "mount_claude_config is true but ~/.agents-in-a-box/auth/.credentials.json not found - run 'agents-box auth' first"
                    );
                }

                // Check for .claude.json in the auth directory (created during OAuth)
                let claude_json_auth_path = home_dir.join(".agents-in-a-box/auth/.claude.json");
                if claude_json_auth_path.exists() {
                    *config = config.clone().with_volume(
                        claude_json_auth_path,
                        "/home/claude-user/.claude.json".to_string(), // Mount to home directory for Claude CLI access
                        false, // read-write mount for Claude CLI organic updates (theme, etc.)
                    );
                    info!(
                        "Mounting agents-in-a-box .claude.json from auth directory to ~/.claude.json"
                    );
                } else {
                    info!(
                        "No .claude.json found in auth directory - theme preferences will be set in container"
                    );
                }

                // Mount .env file if it exists for API key authentication
                let env_path = home_dir.join(".agents-in-a-box/.env");
                if env_path.exists() {
                    *config = config.clone().with_volume(
                        env_path,
                        "/app/.env".to_string(),
                        true, // read-only for security
                    );
                    info!("Mounting agents-in-a-box .env file for API key authentication");
                }
            }
        } else {
            info!("Skipping Claude config mounting (mount_claude_config is false)");
        }

        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::CheckingEnvironment).await;
        }

        Ok(())
    }

    /// Create and start the container
    async fn create_and_start_container(
        &mut self,
        session_id: Uuid,
        config: ContainerConfig,
        progress_sender: &Option<mpsc::Sender<SessionProgress>>,
    ) -> Result<SessionContainer, SessionLifecycleError> {
        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::StartingContainer).await;
        }

        let mut container =
            self.container_manager.create_session_container(session_id, config).await?;

        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::WaitingForContainer).await;
        }

        // Start the container
        self.container_manager.start_container(&mut container).await?;

        if let Some(ref tx) = progress_sender {
            let _ = tx.send(SessionProgress::VerifyingContainer).await;
        }

        info!("Started container for session {}", session_id);
        Ok(container)
    }

    /// Create session state model
    async fn create_session_state(
        &mut self,
        request: SessionRequest,
        container: SessionContainer,
        worktree_info: WorktreeInfo,
    ) -> Result<SessionState, SessionLifecycleError> {
        let mut session = Session::new_with_options(
            format!("{}-{}", request.workspace_name, request.branch_name),
            worktree_info.path.to_string_lossy().to_string(), // Use worktree path, not original repo path
            request.skip_permissions,
            request.mode.clone(),
            request.boss_prompt.clone(),
            request.agent_type,
            request.model,
        );
        session.id = request.session_id;
        session.branch_name = request.branch_name.clone();
        session.container_id = container.container_id.clone();

        // Set session status to Running since the container was successfully created and started
        session.set_status(SessionStatus::Running);

        let session_state = SessionState {
            session,
            container: Some(container),
            worktree_info: Some(worktree_info),
        };

        // Register the session
        self.active_sessions.insert(request.session_id, session_state.clone());

        Ok(session_state)
    }

    /// Create a new session using an existing worktree instead of creating a new one
    /// This is useful for reusing worktrees from previous sessions
    pub async fn create_session_with_existing_worktree(
        &mut self,
        request: SessionRequest,
        existing_worktree: WorktreeInfo,
    ) -> Result<SessionState, SessionLifecycleError> {
        info!(
            "Creating new session {} with existing worktree at {}",
            request.session_id,
            existing_worktree.path.display()
        );

        // Check if session already exists
        if self.active_sessions.contains_key(&request.session_id) {
            return Err(SessionLifecycleError::SessionAlreadyExists(
                request.session_id,
            ));
        }

        // Verify the existing worktree still exists and is valid
        if !existing_worktree.path.exists() {
            return Err(SessionLifecycleError::InvalidState(format!(
                "Existing worktree path does not exist: {}",
                existing_worktree.path.display()
            )));
        }

        // Reuse the existing load_session_configuration helper
        let (project_config, template) = self.load_session_configuration(&request, &None).await?;

        // Create session model using the existing worktree path
        let mut session = Session::new_with_options(
            format!("{}-{}", request.workspace_name, request.branch_name),
            existing_worktree.path.to_string_lossy().to_string(),
            request.skip_permissions,
            request.mode.clone(),
            request.boss_prompt.clone(),
            request.agent_type,
            request.model,
        );
        session.id = request.session_id;
        session.branch_name = request.branch_name.clone();

        // Create base container config using existing helper
        let mut container_config =
            self.create_base_container_config(&template, &existing_worktree, &None).await?;

        // Apply project overrides using existing helper
        self.apply_project_overrides(&mut container_config, &project_config, &request, &None)
            .await?;

        // Initialize MCP servers using existing helper
        let mcp_result = self
            .initialize_mcp_servers(&mut container_config, &request, &project_config, &None)
            .await?;

        // Apply mounting logic using existing helper
        self.apply_mounting_logic(&mut container_config, &project_config, &mcp_result, &None)
            .await?;

        // Remove any existing container for this session first
        // This is necessary when restarting a session - we need to clean up the old container
        let container_name = format!("agents-session-{}", request.session_id);
        info!("Checking for existing container: {}", container_name);

        // Try to remove existing container if it exists
        if let Ok(containers) = self.container_manager.list_agents_containers().await {
            for existing_container in containers {
                if let Some(names) = &existing_container.names {
                    if names.iter().any(|n| n.trim_start_matches('/') == container_name) {
                        info!(
                            "Found existing container for session {}, removing it",
                            request.session_id
                        );
                        if let Some(container_id) = &existing_container.id {
                            // Try to remove the container (this will stop it first if needed)
                            match self.container_manager.remove_container_by_id(container_id).await
                            {
                                Ok(_) => {
                                    info!("Successfully removed old container {}", container_id)
                                }
                                Err(e) => {
                                    warn!("Failed to remove old container {}: {}", container_id, e)
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }

        // Create and start the container using the correct API
        let mut container = self
            .container_manager
            .create_session_container(request.session_id, container_config)
            .await?;

        let container_id = container.container_id.clone().unwrap_or_default();
        session.container_id = Some(container_id.clone());

        // Start the container
        self.container_manager.start_container(&mut container).await?;
        session.set_status(SessionStatus::Running);

        info!(
            "Created container {} for session with existing worktree",
            container_id
        );

        // Create session state
        let session_state = SessionState {
            session,
            worktree_info: Some(existing_worktree),
            container: Some(container),
        };

        // Store the session
        self.active_sessions.insert(request.session_id, session_state.clone());

        info!(
            "Successfully created session {} with existing worktree",
            request.session_id
        );
        Ok(session_state)
    }
}

impl SessionRequest {
    pub fn new(
        session_id: Uuid,
        workspace_name: String,
        workspace_path: PathBuf,
        branch_name: String,
    ) -> Self {
        Self {
            session_id,
            workspace_name,
            workspace_path,
            branch_name,
            base_branch: None,
            container_config: None,
            skip_permissions: false,
            mode: crate::models::SessionMode::Interactive, // Default to interactive mode
            boss_prompt: None,
            agent_type: crate::models::SessionAgentType::Claude, // Default to Claude
            model: None,
        }
    }

    pub fn with_base_branch(mut self, base_branch: String) -> Self {
        self.base_branch = Some(base_branch);
        self
    }

    pub fn with_container_config(mut self, config: ContainerConfig) -> Self {
        self.container_config = Some(config);
        self
    }

    /// Create a request for a Claude development session
    pub fn claude_dev_session(
        session_id: Uuid,
        workspace_name: String,
        workspace_path: PathBuf,
        branch_name: String,
    ) -> Self {
        // Don't specify container_config - let the lifecycle manager use templates
        Self {
            session_id,
            workspace_name,
            workspace_path,
            branch_name,
            base_branch: None,
            container_config: None, // Will use "claude-dev" template by default
            skip_permissions: false,
            mode: crate::models::SessionMode::Interactive, // Default to interactive mode
            boss_prompt: None,
            agent_type: crate::models::SessionAgentType::Claude, // Claude dev session
            model: None,
        }
    }

    /// Create a request with specific container template
    pub fn with_template(
        session_id: Uuid,
        workspace_name: String,
        workspace_path: PathBuf,
        branch_name: String,
        _template_name: String,
    ) -> Self {
        // For now, we'll let the project config specify the template
        // In the future, we could add template selection to SessionRequest
        Self::new(session_id, workspace_name, workspace_path, branch_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Note: These tests require Docker to be running
    // They are integration tests and should be run with `cargo test --ignored`

    #[tokio::test]
    #[ignore]
    async fn test_session_lifecycle_manager_creation() {
        let manager = SessionLifecycleManager::new().await;
        assert!(
            manager.is_ok(),
            "Should be able to create session lifecycle manager"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_session_lifecycle() {
        let mut manager = SessionLifecycleManager::new().await.unwrap();
        let temp_dir = TempDir::new().unwrap();

        // Create a test git repository
        let repo = git2::Repository::init(temp_dir.path()).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Initial commit",
            &tree,
            &[],
        )
        .unwrap();

        let session_id = Uuid::new_v4();
        let request = SessionRequest::new(
            session_id,
            "test-workspace".to_string(),
            temp_dir.path().to_path_buf(),
            "test-branch".to_string(),
        );

        // Create session
        let session_state = manager.create_session(request, None).await.unwrap();
        assert_eq!(session_state.session.id, session_id);
        assert!(session_state.worktree_info.is_some());

        // Start session
        manager.start_session(session_id).await.unwrap();
        let session = manager.get_session(session_id).unwrap();
        assert!(session.session.status.is_running());

        // Stop session
        manager.stop_session(session_id).await.unwrap();
        let session = manager.get_session(session_id).unwrap();
        assert!(!session.session.status.is_running());

        // Remove session
        manager.remove_session(session_id).await.unwrap();
        assert!(manager.get_session(session_id).is_none());
    }

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_agents_dev_session_lifecycle() {
        let mut manager = SessionLifecycleManager::new().await.unwrap();
        let temp_dir = TempDir::new().unwrap();

        // Create a test git repository
        let repo = git2::Repository::init(temp_dir.path()).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();

        // Create a test file
        std::fs::write(
            temp_dir.path().join("test.py"),
            "print('Hello agents-dev!')\n",
        )
        .unwrap();

        let tree_id = {
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("test.py")).unwrap();
            index.write().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Initial commit",
            &tree,
            &[],
        )
        .unwrap();

        let session_id = Uuid::new_v4();
        let request = SessionRequest::claude_dev_session(
            session_id,
            "test-workspace".to_string(),
            temp_dir.path().to_path_buf(),
            "test-branch".to_string(),
        );

        // Create agents-dev session
        let session_state = manager.create_agents_dev_session(request).await.unwrap();
        assert_eq!(session_state.session.id, session_id);
        assert!(session_state.worktree_info.is_some());
        assert!(session_state.session.container_id.is_some());

        // Session should be running (agents-dev starts automatically)
        let session = manager.get_session(session_id).unwrap();
        assert!(session.session.status.is_running());

        // Remove session (includes container cleanup)
        manager.remove_session(session_id).await.unwrap();
        assert!(manager.get_session(session_id).is_none());
    }

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_agents_dev_session_with_progress() {
        let mut manager = SessionLifecycleManager::new().await.unwrap();
        let temp_dir = TempDir::new().unwrap();

        // Create a test git repository
        let repo = git2::Repository::init(temp_dir.path()).unwrap();
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();

        // Create a test file
        std::fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name": "test-workspace"}"#,
        )
        .unwrap();

        let tree_id = {
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("package.json")).unwrap();
            index.write().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Initial commit",
            &tree,
            &[],
        )
        .unwrap();

        let session_id = Uuid::new_v4();
        let request = SessionRequest::claude_dev_session(
            session_id,
            "test-workspace".to_string(),
            temp_dir.path().to_path_buf(),
            "test-branch".to_string(),
        );

        // Create progress channel
        let (tx, mut rx) = mpsc::channel(100);

        // Create agents-dev session with progress tracking
        let session_task = tokio::spawn(async move {
            manager.create_agents_dev_session_with_logs(request, Some(tx)).await
        });

        // Collect progress messages
        let mut progress_messages = Vec::new();
        while let Some(progress) = rx.recv().await {
            let should_break = matches!(progress, AgentsDevProgress::Ready);
            progress_messages.push(progress);
            if should_break {
                break;
            }
        }

        // Wait for session creation
        let session_state = session_task.await.unwrap().unwrap();
        assert_eq!(session_state.session.id, session_id);
        assert!(session_state.worktree_info.is_some());

        // Should have received progress messages
        assert!(!progress_messages.is_empty());

        // Should have authentication sync progress
        let has_auth_sync = progress_messages
            .iter()
            .any(|p| matches!(p, AgentsDevProgress::SyncingAuthentication));
        assert!(has_auth_sync);

        // Should have environment check progress
        let has_env_check = progress_messages
            .iter()
            .any(|p| matches!(p, AgentsDevProgress::CheckingEnvironment));
        assert!(has_env_check);
    }
}
