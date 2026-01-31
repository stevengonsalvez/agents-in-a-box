// ABOUTME: Session data model representing a Claude Code container instance with git worktree

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    Interactive, // Traditional interactive mode with shell access
    Boss,        // Non-interactive mode with direct prompt execution
}

impl Default for SessionMode {
    fn default() -> Self {
        SessionMode::Interactive
    }
}

/// Agent type for the session - which AI agent or shell to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SessionAgentType {
    #[default]
    Claude,
    Shell,  // Plain shell, no AI agent
    Ssh,    // SSH connection to remote server
    Codex,  // Coming soon
    Gemini, // Coming soon
    Kiro,   // Coming soon
}

impl SessionAgentType {
    pub fn icon(&self) -> &'static str {
        match self {
            SessionAgentType::Claude => "🤖",
            SessionAgentType::Shell => "🐚",
            SessionAgentType::Ssh => "🔐",
            SessionAgentType::Codex => "💻",
            SessionAgentType::Gemini => "✨",
            SessionAgentType::Kiro => "🔮",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            SessionAgentType::Claude => "Claude Code",
            SessionAgentType::Shell => "Shell Only",
            SessionAgentType::Ssh => "SSH",
            SessionAgentType::Codex => "Codex CLI",
            SessionAgentType::Gemini => "Gemini CLI",
            SessionAgentType::Kiro => "Kiro",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            SessionAgentType::Claude => "AI coding assistant powered by Anthropic",
            SessionAgentType::Shell => "Plain terminal shell without AI agent",
            SessionAgentType::Ssh => "SSH connection to remote server",
            SessionAgentType::Codex => "OpenAI's coding assistant",
            SessionAgentType::Gemini => "Google's AI assistant",
            SessionAgentType::Kiro => "AWS AI coding assistant",
        }
    }

    pub fn is_available(&self) -> bool {
        match self {
            SessionAgentType::Claude | SessionAgentType::Shell | SessionAgentType::Ssh | SessionAgentType::Codex | SessionAgentType::Gemini => true,
            SessionAgentType::Kiro => false,
        }
    }
}

/// Available Claude models for session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ClaudeModel {
    #[default]
    Sonnet,
    Opus,
    Haiku,
}

impl ClaudeModel {
    /// Get the CLI value to pass to `claude --model`
    pub fn cli_value(&self) -> &'static str {
        match self {
            ClaudeModel::Sonnet => "sonnet",
            ClaudeModel::Opus => "opus",
            ClaudeModel::Haiku => "haiku",
        }
    }

    /// Get human-readable display name
    pub fn display_name(&self) -> &'static str {
        match self {
            ClaudeModel::Sonnet => "Sonnet",
            ClaudeModel::Opus => "Opus",
            ClaudeModel::Haiku => "Haiku",
        }
    }

    /// Get model description for UI
    pub fn description(&self) -> &'static str {
        match self {
            ClaudeModel::Sonnet => "Balanced speed and intelligence (default)",
            ClaudeModel::Opus => "Most capable, best for complex tasks",
            ClaudeModel::Haiku => "Fastest, best for simple tasks",
        }
    }

    /// Get all available models
    pub fn all() -> Vec<ClaudeModel> {
        vec![ClaudeModel::Sonnet, ClaudeModel::Opus, ClaudeModel::Haiku]
    }

    /// Get icon for the model
    pub fn icon(&self) -> &'static str {
        match self {
            ClaudeModel::Sonnet => "⚖️",
            ClaudeModel::Opus => "🎭",
            ClaudeModel::Haiku => "⚡",
        }
    }
}

// ============================================================================
// SSH TARGET (Connection configuration for SSH sessions)
// ============================================================================

/// SSH connection target configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub identity_file: Option<std::path::PathBuf>,
}

impl SshTarget {
    /// Create a new SSH target with just a hostname (default port 22)
    pub fn new(host: String) -> Self {
        Self {
            host,
            port: 22,
            user: None,
            identity_file: None,
        }
    }

    /// Create with full configuration
    pub fn with_config(
        host: String,
        port: u16,
        user: Option<String>,
        identity_file: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            host,
            port,
            user,
            identity_file,
        }
    }

    /// Builder: set port
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Builder: set user
    pub fn with_user(mut self, user: String) -> Self {
        self.user = Some(user);
        self
    }

    /// Build the SSH command string
    pub fn to_ssh_command(&self) -> String {
        let mut cmd = String::from("ssh");

        // Add port if non-default
        if self.port != 22 {
            cmd.push_str(&format!(" -p {}", self.port));
        }

        // Add identity file if specified
        if let Some(ref identity) = self.identity_file {
            cmd.push_str(&format!(" -i {}", identity.display()));
        }

        // Add user@host or just host
        if let Some(ref user) = self.user {
            cmd.push_str(&format!(" {}@{}", user, self.host));
        } else {
            cmd.push_str(&format!(" {}", self.host));
        }

        cmd
    }

    /// Display string for UI (e.g., "user@host:port" or "host")
    pub fn display_name(&self) -> String {
        if let Some(ref user) = self.user {
            if self.port != 22 {
                format!("{}@{}:{}", user, self.host, self.port)
            } else {
                format!("{}@{}", user, self.host)
            }
        } else if self.port != 22 {
            format!("{}:{}", self.host, self.port)
        } else {
            self.host.clone()
        }
    }
}

impl Default for SshTarget {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: None,
            identity_file: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Running,
    Stopped,
    Idle,  // Tmux exists but Claude stopped
    Error(String),
}

impl SessionStatus {
    pub fn indicator(&self) -> &'static str {
        match self {
            SessionStatus::Running => "●",
            SessionStatus::Stopped => "⏸",
            SessionStatus::Idle => "○",  // Empty circle for idle
            SessionStatus::Error(_) => "✗",
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, SessionStatus::Running)
    }

    /// Helper to check if session can be restarted
    pub fn can_restart(&self) -> bool {
        matches!(self, SessionStatus::Idle | SessionStatus::Error(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub name: String,
    pub workspace_path: String,
    pub branch_name: String,
    pub container_id: Option<String>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub git_changes: GitChanges,
    pub recent_logs: Option<String>,
    pub skip_permissions: bool, // Whether to use --dangerously-skip-permissions flag
    pub mode: SessionMode,      // Interactive or Boss mode
    pub boss_prompt: Option<String>, // The prompt for boss mode execution
    #[serde(default)]
    pub agent_type: SessionAgentType, // The AI agent or shell for this session
    #[serde(default)]
    pub model: Option<ClaudeModel>, // Claude model for this session (only for Claude agent)
    #[serde(default)]
    pub ssh_target: Option<SshTarget>, // SSH connection target for SSH agent type

    // Tmux integration fields
    pub tmux_session_name: Option<String>, // Name of the tmux session if using tmux backend
    pub preview_content: Option<String>,   // Cached preview content for display
    pub is_attached: bool,                 // Whether user is currently attached to the session
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitChanges {
    pub added: u32,
    pub modified: u32,
    pub deleted: u32,
}

impl GitChanges {
    pub fn total(&self) -> u32 {
        self.added + self.modified + self.deleted
    }

    pub fn format(&self) -> String {
        if self.total() == 0 {
            "No changes".to_string()
        } else {
            format!("+{} ~{} -{}", self.added, self.modified, self.deleted)
        }
    }
}

// ============================================================================
// SHELL SESSION (Plain terminal without AI agent)
// ============================================================================

/// Status of a shell session
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellSessionStatus {
    Running,  // Tmux session is active
    Detached, // Tmux session exists but not attached
    Stopped,  // Session was killed
}

impl ShellSessionStatus {
    pub fn indicator(&self) -> &'static str {
        match self {
            ShellSessionStatus::Running => "●",
            ShellSessionStatus::Detached => "○",
            ShellSessionStatus::Stopped => "⏸",
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, ShellSessionStatus::Running | ShellSessionStatus::Detached)
    }
}

impl Default for ShellSessionStatus {
    fn default() -> Self {
        ShellSessionStatus::Detached
    }
}

/// A plain shell session (no AI agent) tied to a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellSession {
    pub id: Uuid,
    pub name: String,                    // Display name (e.g., "shell-main", "shell-feature")
    pub tmux_session_name: String,       // Actual tmux session name
    pub workspace_path: std::path::PathBuf, // Repo root this shell belongs to
    pub working_dir: std::path::PathBuf, // Directory shell was opened in (could be worktree)
    pub created_at: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub status: ShellSessionStatus,
    pub preview_content: Option<String>, // Cached preview content for display
}

impl ShellSession {
    /// Create a new shell session
    /// If branch_name is provided, uses it for naming. Otherwise falls back to directory name.
    pub fn new(
        workspace_path: std::path::PathBuf,
        working_dir: std::path::PathBuf,
        branch_name: Option<String>,
    ) -> Self {
        let now = Utc::now();
        let id = Uuid::new_v4();

        // Use branch name if provided, otherwise use directory name
        let base_name = branch_name.unwrap_or_else(|| {
            working_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("shell")
                .to_string()
        });

        // Clean up branch name (remove slashes, limit length)
        let clean_name = base_name
            .replace('/', "-")
            .chars()
            .take(30)
            .collect::<String>();

        let name = format!("shell-{}", clean_name);

        // Generate unique tmux session name (keep it short)
        let short_id = &id.to_string()[..8];
        let tmux_session_name = format!("ainb-sh-{}", short_id);

        Self {
            id,
            name,
            tmux_session_name,
            workspace_path,
            working_dir,
            created_at: now,
            last_accessed: now,
            status: ShellSessionStatus::Detached,
            preview_content: None,
        }
    }

    /// Create with a custom name
    pub fn new_with_name(
        name: String,
        workspace_path: std::path::PathBuf,
        working_dir: std::path::PathBuf,
    ) -> Self {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let short_id = &id.to_string()[..8];
        let tmux_session_name = format!("ainb-shell-{}-{}", name.replace(' ', "-"), short_id);

        Self {
            id,
            name,
            tmux_session_name,
            workspace_path,
            working_dir,
            created_at: now,
            last_accessed: now,
            status: ShellSessionStatus::Detached,
            preview_content: None,
        }
    }

    /// Update last accessed time
    pub fn touch(&mut self) {
        self.last_accessed = Utc::now();
    }

    /// Create a workspace shell (one per workspace, named after workspace)
    pub fn new_workspace_shell(workspace_path: std::path::PathBuf, workspace_name: &str) -> Self {
        let now = Utc::now();
        let id = Uuid::new_v4();

        // Clean workspace name for shell naming
        let clean_name = workspace_name
            .replace('/', "-")
            .replace(' ', "-")
            .chars()
            .take(30)
            .collect::<String>();

        let name = format!("$ {}", clean_name);

        // Generate unique tmux session name
        let short_id = &id.to_string()[..8];
        let tmux_session_name = format!("ainb-ws-{}", short_id);

        Self {
            id,
            name,
            tmux_session_name,
            workspace_path: workspace_path.clone(),
            working_dir: workspace_path,
            created_at: now,
            last_accessed: now,
            status: ShellSessionStatus::Detached,
            preview_content: None,
        }
    }

    /// Update working directory (used when switching to different worktree)
    pub fn set_working_dir(&mut self, dir: std::path::PathBuf) {
        self.working_dir = dir;
        self.touch();
    }
}

impl Session {
    pub fn new(name: String, workspace_path: String) -> Self {
        Self::new_with_options(
            name,
            workspace_path,
            false,
            SessionMode::Interactive,
            None,
            SessionAgentType::default(),
            None,
        )
    }

    pub fn new_with_options(
        name: String,
        workspace_path: String,
        skip_permissions: bool,
        mode: SessionMode,
        boss_prompt: Option<String>,
        agent_type: SessionAgentType,
        model: Option<ClaudeModel>,
    ) -> Self {
        let now = Utc::now();
        let branch_name = format!("ainb/{}", name.replace(' ', "-").to_lowercase());

        Self {
            id: Uuid::new_v4(),
            name,
            workspace_path,
            branch_name,
            container_id: None,
            status: SessionStatus::Stopped,
            created_at: now,
            last_accessed: now,
            git_changes: GitChanges::default(),
            recent_logs: None,
            skip_permissions,
            mode,
            boss_prompt,
            agent_type,
            model,
            ssh_target: None,
            tmux_session_name: None,
            preview_content: None,
            is_attached: false,
        }
    }

    /// Create a new SSH session with a target configuration
    pub fn new_ssh_session(name: String, ssh_target: SshTarget) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            name,
            workspace_path: String::new(), // SSH sessions don't have a workspace
            branch_name: String::new(),    // SSH sessions don't have a branch
            container_id: None,
            status: SessionStatus::Stopped,
            created_at: now,
            last_accessed: now,
            git_changes: GitChanges::default(),
            recent_logs: None,
            skip_permissions: false,
            mode: SessionMode::Interactive,
            boss_prompt: None,
            agent_type: SessionAgentType::Ssh,
            model: None,
            ssh_target: Some(ssh_target),
            tmux_session_name: None,
            preview_content: None,
            is_attached: false,
        }
    }

    pub fn update_last_accessed(&mut self) {
        self.last_accessed = Utc::now();
    }

    pub fn set_status(&mut self, status: SessionStatus) {
        self.status = status;
        self.update_last_accessed();
    }

    pub fn set_container_id(&mut self, container_id: Option<String>) {
        self.container_id = container_id;
        self.update_last_accessed();
    }

    // Tmux integration methods

    /// Get the tmux session name for this session
    /// Format: tmux_{sanitized_name}
    pub fn get_tmux_name(&self) -> String {
        format!(
            "tmux_{}",
            self.name.replace(' ', "_").replace('.', "_").replace('/', "_")
        )
    }

    /// Set the preview content for this session
    pub fn set_preview(&mut self, content: String) {
        self.preview_content = Some(content);
        self.update_last_accessed();
    }

    /// Mark the session as attached
    pub fn mark_attached(&mut self) {
        self.is_attached = true;
        self.update_last_accessed();
    }

    /// Mark the session as detached
    pub fn mark_detached(&mut self) {
        self.is_attached = false;
        self.update_last_accessed();
    }

    /// Set the tmux session name
    pub fn set_tmux_session_name(&mut self, name: String) {
        self.tmux_session_name = Some(name);
        self.update_last_accessed();
    }
}
