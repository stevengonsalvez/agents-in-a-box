// ABOUTME: Configuration management for agents-in-a-box
// Handles application config, container defaults, and MCP server definitions

#![allow(dead_code)]

use anyhow::{Context, Result};
use crate::audit::{self, AuditResult, AuditTrigger};
use dirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub mod container;
pub mod favorites_store;
pub mod mcp;
pub mod mcp_init;
pub mod onboarding;
pub mod presets;
pub mod ssh_display_names;

pub use container::{ContainerTemplate, ContainerTemplateConfig};
pub use favorites_store::{Favorite, FavoritesStore, SourceType as FavoriteSourceType};
pub use mcp::{McpInitStrategy, McpServerConfig};
pub use mcp_init::{McpInitResult, McpInitializer, apply_mcp_init_result};
pub use onboarding::OnboardingConfig;
pub use presets::{PermissionSet, PresetManager, RepositoryPreset, create_default_presets};
pub use ssh_display_names::SshDisplayNameStore;

/// Authentication provider for Claude API
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeAuthProvider {
    /// System authentication (Claude Pro/Max subscription)
    #[default]
    SystemAuth,
    /// Direct API key (pay-as-you-go)
    ApiKey,
    /// Amazon Bedrock (coming soon)
    AmazonBedrock,
    /// Google Vertex AI (coming soon)
    GoogleVertex,
    /// Microsoft Azure Foundry (coming soon)
    AzureFoundry,
    /// GLM on ZAI (coming soon)
    GlmZai,
    /// LLM Gateway (coming soon)
    LlmGateway,
}

impl ClaudeAuthProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaudeAuthProvider::SystemAuth => "system_auth",
            ClaudeAuthProvider::ApiKey => "api_key",
            ClaudeAuthProvider::AmazonBedrock => "amazon_bedrock",
            ClaudeAuthProvider::GoogleVertex => "google_vertex",
            ClaudeAuthProvider::AzureFoundry => "azure_foundry",
            ClaudeAuthProvider::GlmZai => "glm_zai",
            ClaudeAuthProvider::LlmGateway => "llm_gateway",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "system_auth" => ClaudeAuthProvider::SystemAuth,
            "api_key" => ClaudeAuthProvider::ApiKey,
            "amazon_bedrock" => ClaudeAuthProvider::AmazonBedrock,
            "google_vertex" => ClaudeAuthProvider::GoogleVertex,
            "azure_foundry" => ClaudeAuthProvider::AzureFoundry,
            "glm_zai" => ClaudeAuthProvider::GlmZai,
            "llm_gateway" => ClaudeAuthProvider::LlmGateway,
            _ => ClaudeAuthProvider::SystemAuth,
        }
    }
}

/// CLI provider for agent sessions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CliProvider {
    /// Claude Code CLI (default)
    #[default]
    Claude,
    /// OpenAI Codex CLI
    Codex,
    /// Google Gemini CLI
    Gemini,
}

impl CliProvider {
    /// Get the CLI command to run
    pub fn command(&self) -> &'static str {
        match self {
            CliProvider::Claude => "claude",
            CliProvider::Codex => "codex",
            CliProvider::Gemini => "gemini",
        }
    }

    /// Get the environment variable name for API key
    pub fn api_key_env_var(&self) -> &'static str {
        match self {
            CliProvider::Claude => "ANTHROPIC_API_KEY",
            CliProvider::Codex => "OPENAI_API_KEY",
            CliProvider::Gemini => "GEMINI_API_KEY",
        }
    }

    /// Get display name
    pub fn display_name(&self) -> &'static str {
        match self {
            CliProvider::Claude => "Claude Code",
            CliProvider::Codex => "OpenAI Codex",
            CliProvider::Gemini => "Google Gemini",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CliProvider::Claude => "claude",
            CliProvider::Codex => "codex",
            CliProvider::Gemini => "gemini",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "codex" | "openai" => CliProvider::Codex,
            "gemini" | "google" => CliProvider::Gemini,
            _ => CliProvider::Claude,
        }
    }

    /// Get the flag to skip permission prompts for this CLI
    pub fn skip_permissions_flag(&self) -> &'static str {
        match self {
            CliProvider::Claude => "--dangerously-skip-permissions",
            CliProvider::Codex => "--dangerously-bypass-approvals-and-sandbox",
            CliProvider::Gemini => "-y",
        }
    }
}

/// Authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthenticationConfig {
    /// Active CLI provider for agent sessions
    #[serde(default)]
    pub cli_provider: CliProvider,

    /// Claude authentication provider (for Claude-specific auth methods)
    #[serde(default)]
    pub claude_provider: ClaudeAuthProvider,

    /// Default Claude model to use
    #[serde(default = "default_claude_model")]
    pub default_model: String,

    /// GitHub authentication method (for future use)
    #[serde(default)]
    pub github_method: Option<String>,
}

fn default_claude_model() -> String {
    "sonnet".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Application version
    #[serde(default = "default_version")]
    pub version: String,

    /// Authentication configuration
    #[serde(default)]
    pub authentication: AuthenticationConfig,

    /// Default container template to use if none specified
    #[serde(default = "default_container_template")]
    pub default_container_template: String,

    /// Available container templates
    #[serde(default)]
    pub container_templates: HashMap<String, ContainerTemplate>,

    /// MCP server configurations
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,

    /// Workspace defaults
    #[serde(default)]
    pub workspace_defaults: WorkspaceDefaults,

    /// UI preferences
    #[serde(default)]
    pub ui_preferences: UiPreferences,

    /// Docker configuration
    #[serde(default)]
    pub docker: DockerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDefaults {
    /// Default branch prefix for new sessions
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,

    /// Paths to exclude from workspace scanning (substring match)
    #[serde(default)]
    pub exclude_paths: Vec<String>,

    /// Additional paths to scan for git repositories
    /// These are added to the default paths (~/projects, ~/code, etc.)
    #[serde(default)]
    pub workspace_scan_paths: Vec<PathBuf>,

    /// Maximum number of repositories to show in search results (default: 500)
    #[serde(default = "default_max_repositories")]
    pub max_repositories: usize,

    /// Behavior when a target worktree path already exists
    #[serde(default)]
    pub worktree_collision_behavior: WorktreeCollisionBehavior,
}

impl Default for WorkspaceDefaults {
    fn default() -> Self {
        Self {
            branch_prefix: default_branch_prefix(),
            exclude_paths: Vec::new(),
            workspace_scan_paths: Vec::new(),
            max_repositories: default_max_repositories(),
            worktree_collision_behavior: WorktreeCollisionBehavior::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCollisionBehavior {
    AutoRename,
    Error,
}

impl Default for WorktreeCollisionBehavior {
    fn default() -> Self {
        WorktreeCollisionBehavior::AutoRename
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPreferences {
    /// Color theme
    #[serde(default = "default_theme")]
    pub theme: String,

    /// Whether to show container status in UI
    #[serde(default = "default_true")]
    pub show_container_status: bool,

    /// Whether to show git status in UI
    #[serde(default = "default_true")]
    pub show_git_status: bool,

    /// Preferred editor command (e.g., "code", "cursor", "nvim")
    /// If None, falls back to: code -> $EDITOR -> error
    #[serde(default)]
    pub preferred_editor: Option<String>,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            show_container_status: true,
            show_git_status: true,
            preferred_editor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerConfig {
    /// Docker host connection string
    /// Examples:
    /// - unix:///var/run/docker.sock
    /// - tcp://localhost:2376
    /// - npipe:////./pipe/docker_engine
    pub host: Option<String>,

    /// Connection timeout in seconds
    #[serde(default = "default_docker_timeout")]
    pub timeout: u64,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            host: None,
            timeout: default_docker_timeout(),
        }
    }
}

fn default_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn default_container_template() -> String {
    "claude-dev".to_string()
}

fn default_branch_prefix() -> String {
    "agents/".to_string()
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_true() -> bool {
    true
}

fn default_docker_timeout() -> u64 {
    60
}

fn default_max_repositories() -> usize {
    500
}

impl AppConfig {
    /// Load configuration from default locations
    pub fn load() -> Result<Self> {
        // Try loading from multiple locations in order of precedence
        let config_paths = Self::get_config_paths();

        let mut config = Self::default();

        // Load each config file and merge
        for path in config_paths {
            if path.exists() {
                let content = fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read config from {}", path.display()))?;

                let file_config: AppConfig = toml::from_str(&content)
                    .with_context(|| format!("Failed to parse config from {}", path.display()))?;

                config.merge(file_config);
            }
        }

        // Load built-in container templates if none exist
        if config.container_templates.is_empty() {
            config.load_builtin_templates();
        }

        Ok(config)
    }

    /// Save configuration to user config directory
    pub fn save(&self) -> Result<()> {
        let config_dir = Self::get_user_config_dir()?;
        fs::create_dir_all(&config_dir)?;

        let config_path = config_dir.join("config.toml");
        let content = toml::to_string_pretty(self)?;

        match fs::write(&config_path, &content) {
            Ok(()) => {
                // Audit log the successful config save
                audit::audit_config_saved(
                    &config_path.display().to_string(),
                    AuditTrigger::Automatic,
                    AuditResult::Success,
                    None,
                );
                Ok(())
            }
            Err(e) => {
                // Audit log the failed config save
                audit::audit_config_saved(
                    &config_path.display().to_string(),
                    AuditTrigger::Automatic,
                    AuditResult::Failed(e.to_string()),
                    None,
                );
                Err(e.into())
            }
        }
    }

    /// Get configuration file paths in order of precedence
    fn get_config_paths() -> Vec<PathBuf> {
        let mut paths = vec![];

        // 1. Local project config
        if let Ok(cwd) = std::env::current_dir() {
            paths.push(cwd.join(".agents-box").join("config.toml"));
        }

        // 2. User config (~/.agents-in-a-box/config.toml)
        if let Ok(config_dir) = Self::get_user_config_dir() {
            paths.push(config_dir.join("config.toml"));
        }

        // 3. System config
        paths.push(PathBuf::from("/etc/agents-in-a-box/config.toml"));

        paths
    }

    /// Get user configuration directory
    fn get_user_config_dir() -> Result<PathBuf> {
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        let config_dir = home_dir.join(".agents-in-a-box").join("config");
        Ok(config_dir)
    }

    /// Merge another config into this one
    fn merge(&mut self, other: AppConfig) {
        // Don't override version

        // Merge authentication config
        self.authentication.cli_provider = other.authentication.cli_provider;
        self.authentication.claude_provider = other.authentication.claude_provider;
        if other.authentication.default_model != default_claude_model() {
            self.authentication.default_model = other.authentication.default_model;
        }
        if other.authentication.github_method.is_some() {
            self.authentication.github_method = other.authentication.github_method;
        }

        if !other.default_container_template.is_empty() {
            self.default_container_template = other.default_container_template;
        }

        // Merge maps
        self.container_templates.extend(other.container_templates);
        self.mcp_servers.extend(other.mcp_servers);

        // Override workspace defaults if provided
        if other.workspace_defaults.branch_prefix != default_branch_prefix() {
            self.workspace_defaults.branch_prefix = other.workspace_defaults.branch_prefix;
        }
        if !other.workspace_defaults.exclude_paths.is_empty() {
            self.workspace_defaults.exclude_paths = other.workspace_defaults.exclude_paths;
        }
        if !other.workspace_defaults.workspace_scan_paths.is_empty() {
            self.workspace_defaults.workspace_scan_paths =
                other.workspace_defaults.workspace_scan_paths;
        }
        // Always take max_repositories from config if loaded from file
        self.workspace_defaults.max_repositories = other.workspace_defaults.max_repositories;
        if other.workspace_defaults.worktree_collision_behavior
            != WorktreeCollisionBehavior::default()
        {
            self.workspace_defaults.worktree_collision_behavior =
                other.workspace_defaults.worktree_collision_behavior;
        }

        // Override UI preferences
        // Check if this is an old config (empty theme indicates pre-v0.4 config)
        let is_old_config = other.ui_preferences.theme.is_empty();

        if !other.ui_preferences.theme.is_empty() && other.ui_preferences.theme != default_theme() {
            self.ui_preferences.theme = other.ui_preferences.theme;
        }
        // For boolean settings: only override default (true) if config explicitly sets false
        // AND this is NOT an old config with empty defaults
        if !is_old_config {
            // New config: respect explicit settings
            self.ui_preferences.show_container_status = other.ui_preferences.show_container_status;
            self.ui_preferences.show_git_status = other.ui_preferences.show_git_status;
        }
        // Old configs keep the default (true) values
        if other.ui_preferences.preferred_editor.is_some() {
            self.ui_preferences.preferred_editor = other.ui_preferences.preferred_editor;
        }

        // Override Docker settings
        if other.docker.host.is_some() {
            self.docker.host = other.docker.host;
        }
        // Only override timeout if it's non-zero (0 indicates old config with unset value)
        if other.docker.timeout != 0 && other.docker.timeout != default_docker_timeout() {
            self.docker.timeout = other.docker.timeout;
        }
    }

    /// Load built-in container templates
    fn load_builtin_templates(&mut self) {
        // Claude development template (based on claude-docker)
        let claude_dev = ContainerTemplate::claude_dev_default();
        self.container_templates.insert("claude-dev".to_string(), claude_dev);

        // Basic templates
        let node_template = ContainerTemplate::node_default();
        self.container_templates.insert("node".to_string(), node_template);

        let python_template = ContainerTemplate::python_default();
        self.container_templates.insert("python".to_string(), python_template);

        let rust_template = ContainerTemplate::rust_default();
        self.container_templates.insert("rust".to_string(), rust_template);
    }

    /// Get a container template by name
    pub fn get_container_template(&self, name: &str) -> Option<&ContainerTemplate> {
        self.container_templates.get(name)
    }

    /// Get the default container template
    pub fn get_default_container_template(&self) -> Option<&ContainerTemplate> {
        self.container_templates.get(&self.default_container_template)
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        let mut config = Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            authentication: AuthenticationConfig::default(),
            default_container_template: default_container_template(),
            container_templates: HashMap::new(),
            mcp_servers: HashMap::new(),
            workspace_defaults: WorkspaceDefaults::default(),
            ui_preferences: UiPreferences::default(),
            docker: DockerConfig::default(),
        };

        // Load built-in templates
        config.load_builtin_templates();

        config
    }
}

/// Load configuration from environment
pub fn load_from_env() -> HashMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| {
            k.starts_with("AGENTS_BOX_")
                || k.starts_with("CLAUDE_")
                || k.starts_with("ANTHROPIC_")
                || k.starts_with("OPENAI_")
                || k.starts_with("CODEX_")
                || k.starts_with("GEMINI_")
                || k.starts_with("GOOGLE_API_")
        })
        .collect()
}

/// Project-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Container template to use for this project
    pub container_template: Option<String>,

    /// Custom container configuration
    pub container_config: Option<ContainerTemplateConfig>,

    /// Project-specific MCP servers
    #[serde(default)]
    pub mcp_servers: Vec<String>,

    /// Project-specific environment variables
    #[serde(default)]
    pub environment: HashMap<String, String>,

    /// Whether to mount ~/.claude directory
    #[serde(default = "default_true")]
    pub mount_claude_config: bool,

    /// Additional paths to mount from host
    #[serde(default)]
    pub additional_mounts: Vec<MountConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountConfig {
    pub host_path: String,
    pub container_path: String,
    #[serde(default)]
    pub read_only: bool,
}

impl ProjectConfig {
    /// Load project configuration from a directory
    pub fn load_from_dir(dir: &Path) -> Result<Option<Self>> {
        let config_path = dir.join(".agents-box").join("project.toml");
        if !config_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&config_path)?;
        let config: ProjectConfig = toml::from_str(&content)?;
        Ok(Some(config))
    }

    /// Save project configuration to a directory
    pub fn save_to_dir(&self, dir: &Path) -> Result<()> {
        let config_dir = dir.join(".agents-box");
        fs::create_dir_all(&config_dir)?;

        let config_path = config_dir.join("project.toml");
        let content = toml::to_string_pretty(self)?;
        fs::write(&config_path, content)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = AppConfig::default();
        assert_eq!(config.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(config.default_container_template, "claude-dev");
        assert!(!config.container_templates.is_empty());
    }

    #[test]
    fn test_project_config_save_load() {
        let temp_dir = TempDir::new().unwrap();
        let project_config = ProjectConfig {
            container_template: Some("node".to_string()),
            container_config: None,
            mcp_servers: vec!["context7".to_string()],
            environment: HashMap::new(),
            mount_claude_config: true,
            additional_mounts: vec![],
        };

        project_config.save_to_dir(temp_dir.path()).unwrap();
        let loaded = ProjectConfig::load_from_dir(temp_dir.path()).unwrap().unwrap();

        assert_eq!(loaded.container_template, Some("node".to_string()));
        assert_eq!(loaded.mcp_servers, vec!["context7".to_string()]);
    }

    #[test]
    fn test_app_config_serialization_roundtrip() {
        // Create config with all customized fields
        let mut config = AppConfig::default();

        // Set workspace defaults
        config.workspace_defaults.branch_prefix = "custom/".to_string();
        config.workspace_defaults.exclude_paths = vec!["vendor".to_string(), "dist".to_string()];
        config.workspace_defaults.max_repositories = 1000;
        config.workspace_defaults.worktree_collision_behavior = WorktreeCollisionBehavior::Error;

        // Set Docker settings
        config.docker.host = Some("tcp://localhost:2376".to_string());
        config.docker.timeout = 120;

        // Set UI preferences
        config.ui_preferences.theme = "light".to_string();
        config.ui_preferences.show_container_status = false;
        config.ui_preferences.show_git_status = false;
        config.ui_preferences.preferred_editor = Some("nvim".to_string());

        // Serialize to TOML
        let toml_str = toml::to_string_pretty(&config).expect("Failed to serialize config");

        // Verify TOML contains our settings
        assert!(toml_str.contains("branch_prefix = \"custom/\""), "branch_prefix not in TOML");
        assert!(toml_str.contains("vendor"), "exclude_paths not in TOML");
        assert!(toml_str.contains("max_repositories = 1000"), "max_repositories not in TOML");
        assert!(
            toml_str.contains("worktree_collision_behavior = \"error\""),
            "worktree_collision_behavior not in TOML"
        );
        assert!(toml_str.contains("tcp://localhost:2376"), "docker.host not in TOML");
        assert!(toml_str.contains("timeout = 120"), "docker.timeout not in TOML");
        assert!(toml_str.contains("theme = \"light\""), "theme not in TOML");
        assert!(toml_str.contains("show_container_status = false"), "show_container_status not in TOML");
        assert!(toml_str.contains("show_git_status = false"), "show_git_status not in TOML");
        assert!(toml_str.contains("preferred_editor = \"nvim\""), "preferred_editor not in TOML");

        // Deserialize back
        let loaded: AppConfig = toml::from_str(&toml_str).expect("Failed to deserialize config");

        // Verify all fields match
        assert_eq!(loaded.workspace_defaults.branch_prefix, "custom/");
        assert_eq!(loaded.workspace_defaults.exclude_paths, vec!["vendor", "dist"]);
        assert_eq!(loaded.workspace_defaults.max_repositories, 1000);
        assert_eq!(
            loaded.workspace_defaults.worktree_collision_behavior,
            WorktreeCollisionBehavior::Error
        );
        assert_eq!(loaded.docker.host, Some("tcp://localhost:2376".to_string()));
        assert_eq!(loaded.docker.timeout, 120);
        assert_eq!(loaded.ui_preferences.theme, "light");
        assert_eq!(loaded.ui_preferences.show_container_status, false);
        assert_eq!(loaded.ui_preferences.show_git_status, false);
        assert_eq!(loaded.ui_preferences.preferred_editor, Some("nvim".to_string()));
    }

    #[test]
    fn test_app_config_merge_preserves_docker_settings() {
        let mut base = AppConfig::default();
        let mut other = AppConfig::default();

        // Set docker settings in other config
        other.docker.host = Some("unix:///custom/docker.sock".to_string());
        other.docker.timeout = 90;

        // Merge
        base.merge(other);

        // Verify docker settings were merged
        assert_eq!(base.docker.host, Some("unix:///custom/docker.sock".to_string()));
        assert_eq!(base.docker.timeout, 90);
    }
}

#[cfg(test)]
mod old_config_tests {
    use super::*;

    #[test]
    fn test_old_config_merge_keeps_default_true_for_booleans() {
        // Start with defaults (which have true for show_container_status and show_git_status)
        let mut defaults = AppConfig::default();
        assert!(defaults.ui_preferences.show_container_status, "Default should be true");
        assert!(defaults.ui_preferences.show_git_status, "Default should be true");

        // Simulate an "old config" with empty theme and false values
        let old_config = AppConfig {
            ui_preferences: UiPreferences {
                theme: "".to_string(), // Empty theme indicates old config
                show_container_status: false,
                show_git_status: false,
                preferred_editor: None,
            },
            docker: DockerConfig {
                host: None,
                timeout: 0, // 0 indicates old config
            },
            ..AppConfig::default()
        };

        // Merge the old config into defaults
        defaults.merge(old_config);

        // Old config should NOT override the default true values
        assert!(
            defaults.ui_preferences.show_container_status,
            "Old config should not override show_container_status to false"
        );
        assert!(
            defaults.ui_preferences.show_git_status,
            "Old config should not override show_git_status to false"
        );

        // Old config timeout=0 should be ignored, keeping default (60)
        assert_eq!(
            defaults.docker.timeout, 60,
            "Old config timeout=0 should be ignored, keeping default"
        );
    }

    #[test]
    fn test_new_config_merge_respects_explicit_false() {
        let mut defaults = AppConfig::default();

        // Simulate a "new config" with non-empty theme and explicit false values
        let new_config = AppConfig {
            ui_preferences: UiPreferences {
                theme: "light".to_string(), // Non-empty theme indicates new config
                show_container_status: false,
                show_git_status: false,
                preferred_editor: None,
            },
            docker: DockerConfig {
                host: None,
                timeout: 30, // Non-zero timeout
            },
            ..AppConfig::default()
        };

        defaults.merge(new_config);

        // New config should override the defaults with explicit false
        assert!(
            !defaults.ui_preferences.show_container_status,
            "New config should be able to set show_container_status to false"
        );
        assert!(
            !defaults.ui_preferences.show_git_status,
            "New config should be able to set show_git_status to false"
        );

        // Theme should be updated
        assert_eq!(defaults.ui_preferences.theme, "light");

        // Timeout should be updated
        assert_eq!(defaults.docker.timeout, 30);
    }
}
