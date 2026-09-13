// ABOUTME: Agents-dev container management module
// Handles authentication, environment setup, and container operations for agents-dev sessions

#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::builder::ImageBuilder;
use super::container_manager::ContainerManager;

/// Configuration for agents-dev container setup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsDevConfig {
    /// Container image name
    pub image_name: String,
    /// Memory limit (e.g., "4g", "2048m")
    pub memory_limit: Option<String>,
    /// GPU access (e.g., "all", "device=0")
    pub gpu_access: Option<String>,
    /// Whether to force rebuild image
    pub force_rebuild: bool,
    /// Whether to build without cache
    pub no_cache: bool,
    /// Whether to continue from last session
    pub continue_session: bool,
    /// Whether to skip permission prompts
    pub skip_permissions: bool,
    /// Environment variables to pass to container
    pub env_vars: HashMap<String, String>,
}

impl Default for AgentsDevConfig {
    fn default() -> Self {
        Self {
            image_name: "agents-box:agents-dev".to_string(),
            memory_limit: None,
            gpu_access: None,
            force_rebuild: false,
            no_cache: false,
            continue_session: false,
            skip_permissions: false,
            env_vars: HashMap::new(),
        }
    }
}

/// Authentication status for agents-dev
#[derive(Debug, Clone)]
pub struct AuthenticationStatus {
    pub claude_json_exists: bool,
    pub credentials_json_exists: bool,
    pub anthropic_api_key_set: bool,
    pub github_token_set: bool,
    pub sources: Vec<String>,
}

/// Progress updates for agents-dev operations
#[derive(Debug, Clone)]
pub enum AgentsDevProgress {
    SyncingAuthentication,
    CheckingEnvironment,
    BuildingImage(String),
    StartingContainer,
    ConfiguringGitHub,
    Ready,
    Error(String),
}

/// Main agents-dev container manager
pub struct AgentsDevManager {
    config: AgentsDevConfig,
    container_manager: ContainerManager,
    image_builder: ImageBuilder,
    claude_home_dir: PathBuf,
    ssh_dir: PathBuf,
}

impl AgentsDevManager {
    /// Create new agents-dev manager
    pub async fn new(config: AgentsDevConfig) -> Result<Self> {
        let container_manager = ContainerManager::new().await?;
        let image_builder = ImageBuilder::new().await?;

        // Setup agents-box directories
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        // Mount the actual .claude directory for proper authentication
        let claude_home_dir = home_dir.join(".claude");
        let ssh_dir = home_dir.join(".ssh");

        // Only create claude directory if it doesn't exist (be careful with user's actual config)
        if !claude_home_dir.exists() {
            std::fs::create_dir_all(&claude_home_dir)?;
        }
        // SSH directory is read from host, don't create it

        Ok(Self {
            config,
            container_manager,
            image_builder,
            claude_home_dir,
            ssh_dir,
        })
    }

    /// Get authentication status
    pub fn get_authentication_status(&self) -> Result<AuthenticationStatus> {
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        let mut sources = Vec::new();

        // Check for .claude.json in home directory
        let claude_json_path = home_dir.join(".claude.json");
        let claude_json_exists =
            claude_json_path.exists() && claude_json_path.metadata()?.len() > 0;
        if claude_json_exists {
            sources.push(".claude.json (host)".to_string());
        }

        // Check for .credentials.json in .claude directory
        let credentials_path = self.claude_home_dir.join(".credentials.json");
        let credentials_json_exists =
            credentials_path.exists() && credentials_path.metadata()?.len() > 0;
        if credentials_json_exists {
            sources.push(".claude/.credentials.json (host)".to_string());
        }

        // Check for environment variables
        let anthropic_api_key_set = std::env::var("ANTHROPIC_API_KEY").is_ok();
        if anthropic_api_key_set {
            sources.push("ANTHROPIC_API_KEY environment variable".to_string());
        }

        let github_token_set = std::env::var("GITHUB_TOKEN").is_ok()
            || self.config.env_vars.contains_key("GITHUB_TOKEN");
        if github_token_set {
            sources.push("GITHUB_TOKEN environment variable".to_string());
        }

        Ok(AuthenticationStatus {
            claude_json_exists,
            credentials_json_exists,
            anthropic_api_key_set,
            github_token_set,
            sources,
        })
    }

    /// Check authentication files exist (no syncing needed since we mount directly)
    pub async fn sync_authentication_files(
        &self,
        progress_tx: Option<mpsc::Sender<AgentsDevProgress>>,
    ) -> Result<()> {
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(AgentsDevProgress::SyncingAuthentication).await;
        }

        // Just verify authentication files exist
        let auth_status = self.get_authentication_status()?;
        if auth_status.sources.is_empty() {
            warn!(
                "No Claude authentication found. Please ensure ~/.claude.json or ~/.claude/.credentials.json exists"
            );
        } else {
            info!("Claude authentication found: {:?}", auth_status.sources);
        }

        Ok(())
    }

    /// Setup environment variables and GitHub CLI configuration
    pub async fn setup_environment(
        &self,
        progress_tx: Option<mpsc::Sender<AgentsDevProgress>>,
    ) -> Result<()> {
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(AgentsDevProgress::CheckingEnvironment).await;
        }

        // Check for GITHUB_TOKEN
        let github_token = std::env::var("GITHUB_TOKEN").or_else(|_| {
            self.config
                .env_vars
                .get("GITHUB_TOKEN")
                .cloned()
                .ok_or_else(|| std::env::VarError::NotPresent)
        });

        if let Ok(_token) = github_token {
            info!("GITHUB_TOKEN found - will use token-based authentication");
            debug!("GitHub CLI and token-based git operations will be available");
        } else {
            warn!("GITHUB_TOKEN not found");
            info!("To enable full GitHub integration:");
            info!("  1. Create GitHub Personal Access Token:");
            info!("     https://github.com/settings/tokens/new");
            info!("     Required scopes: repo, read:org, workflow");
            info!("  2. Set GITHUB_TOKEN environment variable");

            // Check for SSH keys as fallback
            let ssh_key_path = self.ssh_dir.join("id_rsa");
            let ssh_pub_key_path = self.ssh_dir.join("id_rsa.pub");

            if ssh_key_path.exists() && ssh_pub_key_path.exists() {
                info!("SSH keys found as fallback for git operations");
                self.setup_ssh_config().await?;
            } else {
                info!("Alternative: Generate SSH keys:");
                info!("  ssh-keygen -t rsa -b 4096 -f ~/.claude-box/ssh/id_rsa -N ''");
                info!("  Then add public key to GitHub/GitLab");
                info!("Note: GITHUB_TOKEN is recommended for better integration");
            }
        }

        Ok(())
    }

    /// Build agents-dev Docker image if needed
    pub async fn build_image_if_needed(
        &self,
        progress_tx: Option<mpsc::Sender<AgentsDevProgress>>,
    ) -> Result<()> {
        let need_rebuild =
            self.config.force_rebuild || !self.image_exists(&self.config.image_name).await?;

        if need_rebuild {
            if let Some(ref tx) = progress_tx {
                let _ = tx
                    .send(AgentsDevProgress::BuildingImage(
                        "Starting build...".to_string(),
                    ))
                    .await;
            }

            info!("Building agents-dev image: {}", self.config.image_name);

            // Get current user UID/GID
            let uid = nix::unistd::getuid().as_raw();
            let gid = nix::unistd::getgid().as_raw();

            // Build arguments
            let mut build_args = vec![
                ("HOST_UID".to_string(), uid.to_string()),
                ("HOST_GID".to_string(), gid.to_string()),
            ];

            // Add environment variables if they exist
            if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
                build_args.push(("ANTHROPIC_API_KEY".to_string(), api_key));
            }

            // Build the image
            let dockerfile_dir = PathBuf::from("docker/agents-dev");
            let build_options = super::builder::BuildOptions {
                dockerfile_path: Some(dockerfile_dir.join("Dockerfile")),
                context_path: dockerfile_dir,
                build_args,
                no_cache: self.config.no_cache,
                target: None,
                labels: vec![],
                pull: false,
            };

            // Create progress sender for image build
            let (build_tx, mut build_rx) = mpsc::channel(100);
            let progress_tx_clone = progress_tx.clone();

            // Spawn task to forward build progress
            if progress_tx.is_some() {
                tokio::spawn(async move {
                    while let Some(log) = build_rx.recv().await {
                        if let Some(ref tx) = progress_tx_clone {
                            let _ = tx.send(AgentsDevProgress::BuildingImage(log)).await;
                        }
                    }
                });
            }

            self.image_builder
                .build_image(&self.config.image_name, &build_options, Some(build_tx))
                .await?;

            info!("Successfully built agents-dev image");
        } else {
            debug!(
                "Image {} already exists, skipping build",
                self.config.image_name
            );
        }

        Ok(())
    }

    /// Run agents-dev container
    pub async fn run_container(
        &self,
        workspace_path: &Path,
        session_id: Uuid,
        progress_tx: Option<mpsc::Sender<AgentsDevProgress>>,
        mount_claude_config: bool,
    ) -> Result<String> {
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(AgentsDevProgress::StartingContainer).await;
        }

        info!("Starting agents-dev container");
        info!("Container: {}", self.config.image_name);
        info!("Workspace: {}", workspace_path.display());

        // Prepare container configuration
        let mut env_vars = self.config.env_vars.clone();
        env_vars.insert("AGENTS_BOX_MODE".to_string(), "true".to_string());

        // Add continue flag if requested
        if self.config.continue_session {
            env_vars.insert("CLAUDE_CONTINUE_FLAG".to_string(), "--continue".to_string());
        }

        // Add dangerously-skip-permissions flag if requested
        if self.config.skip_permissions {
            let current_flag = env_vars.get("CLAUDE_CONTINUE_FLAG").cloned().unwrap_or_default();
            let new_flag = if current_flag.is_empty() {
                "--dangerously-skip-permissions".to_string()
            } else {
                format!("{} --dangerously-skip-permissions", current_flag)
            };
            env_vars.insert("CLAUDE_CONTINUE_FLAG".to_string(), new_flag);
        }

        // Setup volume mounts
        let mut mounts = vec![
            // Workspace mount
            (workspace_path.to_path_buf(), PathBuf::from("/workspace")),
            // Claude home directory
            (
                self.claude_home_dir.clone(),
                PathBuf::from("/home/claude-user/.claude"),
            ),
            // SSH directory
            (
                self.ssh_dir.clone(),
                PathBuf::from("/home/claude-user/.ssh"),
            ),
        ];

        // Mount .claude.json from home directory if it exists and mount_claude_config is true
        if mount_claude_config {
            let home_dir = dirs::home_dir().context("Failed to get home directory")?;
            let claude_json_path = home_dir.join(".claude.json");
            if claude_json_path.exists() {
                mounts.push((
                    claude_json_path,
                    PathBuf::from("/home/claude-user/.claude.json"),
                ));
                info!("Mounting .claude.json as read-write for Claude CLI organic updates");
            } else {
                warn!("mount_claude_config is true but ~/.claude.json not found");
            }
        } else {
            info!("Skipping .claude.json mount (mount_claude_config is false)");
        }

        // Container run options
        let mut labels = std::collections::HashMap::new();
        labels.insert("agents-session-id".to_string(), session_id.to_string());

        let run_options = super::container_manager::RunOptions {
            image: self.config.image_name.clone(),
            command: vec![],
            env_vars,
            mounts,
            working_dir: Some("/workspace".to_string()),
            user: None,
            network: None,
            ports: vec![],
            remove_on_exit: true,
            interactive: true,
            tty: true,
            memory_limit: self.config.memory_limit.clone(),
            cpu_limit: None,
            gpu_access: self.config.gpu_access.clone(),
            labels,
        };

        // Generate container name with session ID
        let container_name = format!("agents-session-{}", session_id);

        // Run the container
        let container_id =
            self.container_manager.run_container(&container_name, &run_options).await?;

        if let Some(ref tx) = progress_tx {
            let _ = tx.send(AgentsDevProgress::Ready).await;
        }

        info!(
            "Agents-dev container started successfully: {}",
            container_id
        );
        Ok(container_id)
    }

    /// Check if Docker image exists
    async fn image_exists(&self, image_name: &str) -> Result<bool> {
        let output = Command::new("docker")
            .args(&["images", "-q", image_name])
            .output()
            .context("Failed to check if image exists")?;

        Ok(!output.stdout.is_empty())
    }

    /// Check if first file is newer than second file
    fn is_newer(&self, file1: &Path, file2: &Path) -> Result<bool> {
        if !file2.exists() {
            return Ok(true);
        }

        let metadata1 = file1.metadata()?;
        let metadata2 = file2.metadata()?;

        Ok(metadata1.modified()? > metadata2.modified()?)
    }

    /// Sync directory contents recursively
    fn sync_directory<'a>(
        &'a self,
        source: &'a Path,
        dest: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if !dest.exists() {
                tokio::fs::create_dir_all(dest).await?;
            }

            let mut entries = tokio::fs::read_dir(source).await?;
            while let Some(entry) = entries.next_entry().await? {
                let file_name = entry.file_name();
                let source_path = entry.path();
                let dest_path = dest.join(&file_name);

                if source_path.is_file() {
                    tokio::fs::copy(&source_path, &dest_path).await?;
                } else if source_path.is_dir() {
                    self.sync_directory(&source_path, &dest_path).await?;
                }
            }

            Ok(())
        })
    }

    /// Setup SSH configuration
    async fn setup_ssh_config(&self) -> Result<()> {
        let ssh_config_path = self.ssh_dir.join("config");
        if !ssh_config_path.exists() {
            let config_content = r#"Host github.com
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_rsa
    IdentitiesOnly yes

Host gitlab.com
    HostName gitlab.com
    User git
    IdentityFile ~/.ssh/id_rsa
    IdentitiesOnly yes
"#;
            tokio::fs::write(&ssh_config_path, config_content).await?;
            info!("SSH config created");
        }
        Ok(())
    }
}

/// Helper function to create an agents-dev session
pub async fn create_agents_dev_session(
    workspace_path: &Path,
    config: AgentsDevConfig,
    session_id: Uuid,
    progress_tx: Option<mpsc::Sender<AgentsDevProgress>>,
    mount_claude_config: bool,
) -> Result<String> {
    let manager = AgentsDevManager::new(config).await?;

    // Sync authentication files
    manager.sync_authentication_files(progress_tx.clone()).await?;

    // Setup environment
    manager.setup_environment(progress_tx.clone()).await?;

    // Build image if needed
    manager.build_image_if_needed(progress_tx.clone()).await?;

    // Run container
    let container_id = manager
        .run_container(workspace_path, session_id, progress_tx, mount_claude_config)
        .await?;

    Ok(container_id)
}
