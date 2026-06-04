// ABOUTME: Session data model representing a Claude Code container instance with git worktree

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    // PascalCase variants are the canonical wire format for
    // `~/.agents-in-a-box/sessions.json` (existing on-disk corpus). The
    // lowercase aliases keep the same enum compatible with the preset TOML
    // files (`mode = "boss"` / `mode = "interactive"`), so `RepositoryPreset`
    // can target this single enum instead of a parallel copy.
    #[serde(alias = "interactive")]
    Interactive, // Traditional interactive mode with shell access
    #[serde(alias = "boss")]
    Boss, // Non-interactive mode with direct prompt execution
}

impl Default for SessionMode {
    fn default() -> Self {
        SessionMode::Interactive
    }
}

/// Agent type for the session - which AI agent or shell to use.
///
/// **Phase 2c note:** The canonical session-agent surface now lives in
/// `crate::agents` (trait `SessionAgent` + `SessionAgentRegistry`). This
/// enum is retained as the serialisation type for `~/.agents-in-a-box/
/// sessions.json` — its existing tags ("Claude", "Shell", …) are remapped
/// to lowercase ids by the registry. Plugin-supplied session agents in
/// Phase 4 register straight into the registry without an enum variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SessionAgentType {
    #[default]
    Claude,
    Shell,   // Plain shell, no AI agent
    Ssh,     // SSH connection to remote server
    Codex,   // OpenAI Codex CLI
    Gemini,  // Google Gemini CLI
    Copilot, // GitHub Copilot CLI
    Kiro,    // AWS Kiro (disabled)
}

impl SessionAgentType {
    pub fn icon(&self) -> &'static str {
        match self {
            SessionAgentType::Claude => "✻", // Claude's own starburst/pinwheel from its spinner
            SessionAgentType::Shell => "🐚",
            SessionAgentType::Ssh => "🔐",
            SessionAgentType::Codex => "✦", // OpenAI geometric 4-pointed star
            SessionAgentType::Gemini => "✨",
            SessionAgentType::Copilot => "🐙",
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
            SessionAgentType::Copilot => "GitHub Copilot",
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
            SessionAgentType::Copilot => "GitHub Copilot CLI — AI coding agent by GitHub",
            SessionAgentType::Kiro => "AWS AI coding assistant",
        }
    }

    pub fn is_available(&self) -> bool {
        match self {
            SessionAgentType::Claude
            | SessionAgentType::Shell
            | SessionAgentType::Ssh
            | SessionAgentType::Codex
            | SessionAgentType::Gemini
            | SessionAgentType::Copilot => true,
            SessionAgentType::Kiro => false,
        }
    }

    /// Stable string id matching the agent's entry in
    /// `crate::agents::SessionAgentRegistry`.
    pub fn id(&self) -> &'static str {
        match self {
            SessionAgentType::Claude => "claude",
            SessionAgentType::Shell => "shell",
            SessionAgentType::Ssh => "ssh",
            SessionAgentType::Codex => "codex",
            SessionAgentType::Gemini => "gemini",
            SessionAgentType::Copilot => "copilot",
            SessionAgentType::Kiro => "kiro",
        }
    }

    /// Look up the matching `SessionAgent` trait object via the built-in
    /// registry. Lets call sites move to registry-keyed dispatch
    /// incrementally; new code should prefer `SessionAgentRegistry` directly.
    pub fn as_session_agent(&self) -> std::sync::Arc<dyn crate::agents::SessionAgent> {
        crate::agents::SessionAgentRegistry::built_ins()
            .get(self.id())
            .expect("built-in session-agent registry is missing a known id")
    }
}

/// Available Claude models for session.
///
/// The `SystemDefault` variant is special: when selected, `--model` is omitted
/// from the launched CLI command entirely so the user's `claude` defaults
/// apply. Real model variants serialize their full canonical IDs (e.g.
/// `claude-opus-4-7`, not the `opus` alias) — but `parse()` still accepts the
/// short aliases so existing user-saved presets (`agent_model = "opus"`)
/// continue to deserialize correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ClaudeModel {
    /// Omit `--model` from the spawned `claude` command; user's CLI default wins.
    #[default]
    SystemDefault,
    /// `claude-opus-4-7` — 1M ctx, flagship.
    Opus,
    /// `claude-opus-4-6` — 1M ctx, previous Opus (still live).
    Opus46,
    /// `claude-sonnet-4-6` — 1M ctx, balanced.
    Sonnet,
    /// `claude-haiku-4-5` — 200K ctx, fastest.
    Haiku,
    /// `opusplan` — hybrid (Opus plan-mode + Sonnet exec).
    OpusPlan,
}

impl ClaudeModel {
    /// CLI value to pass to `claude --model`. `None` means "omit the flag entirely".
    pub fn cli_value(&self) -> Option<&'static str> {
        match self {
            ClaudeModel::SystemDefault => None,
            ClaudeModel::Opus => Some("claude-opus-4-7"),
            ClaudeModel::Opus46 => Some("claude-opus-4-6"),
            ClaudeModel::Sonnet => Some("claude-sonnet-4-6"),
            ClaudeModel::Haiku => Some("claude-haiku-4-5"),
            ClaudeModel::OpusPlan => Some("opusplan"),
        }
    }

    /// Human-readable label for the Configure row. Full ID + ctx hint for real
    /// variants; lowercase "system default" for the no-flag variant (rendered
    /// muted-gray italic in the screen).
    pub fn display_label(&self) -> &'static str {
        match self {
            ClaudeModel::SystemDefault => "system default",
            ClaudeModel::Opus => "claude-opus-4-7 [1M]",
            ClaudeModel::Opus46 => "claude-opus-4-6 [1M]",
            ClaudeModel::Sonnet => "claude-sonnet-4-6 [1M]",
            ClaudeModel::Haiku => "claude-haiku-4-5 [200K]",
            ClaudeModel::OpusPlan => "opusplan (hybrid)",
        }
    }

    /// Back-compat alias for older call sites that still ask for `display_name`.
    /// Forwards to `display_label`.
    pub fn display_name(&self) -> &'static str {
        self.display_label()
    }

    /// Get model description for UI
    pub fn description(&self) -> &'static str {
        match self {
            ClaudeModel::SystemDefault => "Use the CLI's built-in default model",
            ClaudeModel::Opus => "Most capable, best for complex reasoning",
            ClaudeModel::Opus46 => "Previous flagship Opus, still live",
            ClaudeModel::Sonnet => "Balanced speed and intelligence",
            ClaudeModel::Haiku => "Fastest, best for simple tasks",
            ClaudeModel::OpusPlan => "Opus for planning, Sonnet for execution",
        }
    }

    /// All variants in the order the Configure ring should cycle them.
    pub fn all() -> Vec<ClaudeModel> {
        vec![
            ClaudeModel::SystemDefault,
            ClaudeModel::Opus,
            ClaudeModel::Opus46,
            ClaudeModel::Sonnet,
            ClaudeModel::Haiku,
            ClaudeModel::OpusPlan,
        ]
    }

    /// Get icon for the model
    pub fn icon(&self) -> &'static str {
        match self {
            ClaudeModel::SystemDefault => "·",
            ClaudeModel::Opus => "🎭",
            ClaudeModel::Opus46 => "🎭",
            ClaudeModel::Sonnet => "⚖️",
            ClaudeModel::Haiku => "⚡",
            ClaudeModel::OpusPlan => "📐",
        }
    }

    /// Parse a TOML / preset string into a `ClaudeModel`. Accepts:
    ///   * `""` or `"default"` → `SystemDefault`
    ///   * Canonical IDs (`claude-opus-4-7`, `claude-sonnet-4-6`, `claude-haiku-4-5`, `opusplan`)
    ///   * Legacy short aliases (`opus`, `sonnet`, `haiku`) so user-saved
    ///     presets written before the 2026-05 refresh still resolve.
    /// Unknown values fall back to `SystemDefault` and emit a tracing::warn.
    pub fn parse(value: &str) -> ClaudeModel {
        match value.trim().to_lowercase().as_str() {
            "" | "default" => ClaudeModel::SystemDefault,
            "opus" | "claude-opus" | "claude-3-opus" | "claude-opus-4-7" => ClaudeModel::Opus,
            "opus-4-6" | "claude-opus-4-6" | "opus46" => ClaudeModel::Opus46,
            "sonnet" | "claude-sonnet" | "claude-3-sonnet" | "claude-sonnet-4-6" => {
                ClaudeModel::Sonnet
            }
            "haiku" | "claude-haiku" | "claude-3-haiku" | "claude-haiku-4-5" => ClaudeModel::Haiku,
            "opusplan" | "opus-plan" => ClaudeModel::OpusPlan,
            other => {
                tracing::warn!(
                    value = %other,
                    "ClaudeModel::parse: unknown model id, defaulting to SystemDefault"
                );
                ClaudeModel::SystemDefault
            }
        }
    }
}

/// Available Codex models for session.
///
/// As with `ClaudeModel`, `SystemDefault` means "omit `--model` from the
/// spawned `codex` command". The Codex CLI's own internal default applies in
/// that case (currently `gpt-5.5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CodexModel {
    /// Omit `--model` from the spawned `codex` command.
    #[default]
    SystemDefault,
    /// `gpt-5.5` — 1M ctx, recommended default.
    Gpt55,
    /// `gpt-5.4` — 1M ctx, flagship alt.
    Gpt54,
    /// `gpt-5.4-mini` — 200K ctx, fast/cheap.
    Gpt54Mini,
    /// `gpt-5.3-codex` — 200K ctx, deep SWE.
    Gpt53Codex,
}

impl CodexModel {
    /// CLI value to pass to `codex --model`. `None` means "omit the flag entirely".
    pub fn cli_value(&self) -> Option<&'static str> {
        match self {
            CodexModel::SystemDefault => None,
            CodexModel::Gpt55 => Some("gpt-5.5"),
            CodexModel::Gpt54 => Some("gpt-5.4"),
            CodexModel::Gpt54Mini => Some("gpt-5.4-mini"),
            CodexModel::Gpt53Codex => Some("gpt-5.3-codex"),
        }
    }

    /// Human-readable label for the Configure row.
    pub fn display_label(&self) -> &'static str {
        match self {
            CodexModel::SystemDefault => "system default",
            CodexModel::Gpt55 => "gpt-5.5 [1M]",
            CodexModel::Gpt54 => "gpt-5.4 [1M]",
            CodexModel::Gpt54Mini => "gpt-5.4-mini [200K]",
            CodexModel::Gpt53Codex => "gpt-5.3-codex [200K]",
        }
    }

    /// All variants in the order the Configure ring should cycle them.
    pub fn all() -> Vec<CodexModel> {
        vec![
            CodexModel::SystemDefault,
            CodexModel::Gpt55,
            CodexModel::Gpt54,
            CodexModel::Gpt54Mini,
            CodexModel::Gpt53Codex,
        ]
    }

    /// Parse a TOML / preset string into a `CodexModel`. Accepts canonical IDs
    /// only (Codex CLI never had short aliases) plus `""` / `"default"` for
    /// the SystemDefault variant. Unknown values fall back to `SystemDefault`.
    pub fn parse(value: &str) -> CodexModel {
        match value.trim().to_lowercase().as_str() {
            "" | "default" => CodexModel::SystemDefault,
            "gpt-5.5" => CodexModel::Gpt55,
            "gpt-5.4" => CodexModel::Gpt54,
            "gpt-5.4-mini" => CodexModel::Gpt54Mini,
            "gpt-5.3-codex" => CodexModel::Gpt53Codex,
            other => {
                tracing::warn!(
                    value = %other,
                    "CodexModel::parse: unknown model id, defaulting to SystemDefault"
                );
                CodexModel::SystemDefault
            }
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
    Idle, // Tmux exists but Claude stopped
    Error(String),
}

impl SessionStatus {
    pub fn indicator(&self) -> &'static str {
        match self {
            SessionStatus::Running => "●",
            SessionStatus::Stopped => "⏸",
            SessionStatus::Idle => "○", // Empty circle for idle
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
    /// Codex model (only meaningful when `agent_type == Codex`). Mirrors
    /// `model` for the Claude agent: `Some(SystemDefault)` and `None` both
    /// cause `--model` to be omitted from the spawned `codex` command;
    /// anything else emits `--model <id>`.
    #[serde(default)]
    pub codex_model: Option<CodexModel>,
    #[serde(default)]
    pub ssh_target: Option<SshTarget>, // SSH connection target for SSH agent type
    #[serde(default)]
    pub display_name: Option<String>, // Custom display name (overrides auto-generated name in UI)

    // Tmux integration fields
    pub tmux_session_name: Option<String>, // Name of the tmux session if using tmux backend
    pub preview_content: Option<String>,   // Cached preview content for display
    pub is_attached: bool,                 // Whether user is currently attached to the session

    /// Live "needs you" marker, recomputed every preview refresh:
    /// `Some(WaitingOnUser)` whenever the agent is **not generating**
    /// (turn ended / idle / parked at a prompt), `None` while it is
    /// actively generating. Drives the amber `[?]` in the session list.
    /// Transient — never persisted; set in `AppState::update_tmux_previews`.
    #[serde(skip)]
    pub live_attention: Option<ainb_plugin_notifyd::AlertKind>,
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
        matches!(
            self,
            ShellSessionStatus::Running | ShellSessionStatus::Detached
        )
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
    pub name: String, // Display name (e.g., "shell-main", "shell-feature")
    pub tmux_session_name: String, // Actual tmux session name
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
            working_dir.file_name().and_then(|n| n.to_str()).unwrap_or("shell").to_string()
        });

        // Clean up branch name (remove slashes, limit length)
        let clean_name = base_name.replace('/', "-").chars().take(30).collect::<String>();

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
            codex_model: None,
            ssh_target: None,
            display_name: None,
            tmux_session_name: None,
            preview_content: None,
            is_attached: false,
            live_attention: None,
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
            codex_model: None,
            ssh_target: Some(ssh_target),
            display_name: None,
            tmux_session_name: None,
            preview_content: None,
            is_attached: false,
            live_attention: None,
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
