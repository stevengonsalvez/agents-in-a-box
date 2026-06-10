// ABOUTME: Configuration management for agents-in-a-box
// Handles application config, container defaults, and MCP server definitions

#![allow(dead_code)]

use crate::audit::{self, AuditResult, AuditTrigger};
use anyhow::{Context, Result};
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
pub mod session_defaults;
pub mod ssh_display_names;

pub use container::{ContainerTemplate, ContainerTemplateConfig};
pub use favorites_store::{
    DeriveFavoriteError, Favorite, FavoritesStore, MigrationReport,
    SourceType as FavoriteSourceType, favorite_from_local_repo,
};
pub use mcp::{McpInitStrategy, McpInstallation, McpServerConfig, McpServerDefinition};
pub use mcp_init::{McpInitResult, McpInitializer, apply_mcp_init_result};
pub use onboarding::OnboardingConfig;
pub use presets::{PermissionSet, PresetManager, RepositoryPreset, create_default_presets};
pub use session_defaults::{PerRepoDefaults, SessionDefaults};
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

/// CLI provider for agent sessions.
///
/// **Phase 2c note:** The canonical provider surface now lives in
/// `crate::providers` (trait `Provider` + `ProviderRegistry`). This enum is
/// retained as the serialisation type for `~/.agents-in-a-box/config/config.toml`
/// — `serde(rename_all = "snake_case")` already gives us "claude" / "codex" /
/// "gemini" / "copilot" string tags, which match the registry ids 1:1, so
/// configs round-trip without migration.
///
/// Use `CliProvider::as_provider(&self)` to look up the trait object and
/// dispatch through the registry. New plugin-supplied providers register
/// directly with `ProviderRegistry` and have no `CliProvider` variant.
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
    /// GitHub Copilot CLI
    Copilot,
}

impl CliProvider {
    /// Get the CLI command to run
    pub fn command(&self) -> &'static str {
        match self {
            CliProvider::Claude => "claude",
            CliProvider::Codex => "codex",
            CliProvider::Gemini => "gemini",
            CliProvider::Copilot => "copilot",
        }
    }

    /// Get the environment variable name for API key
    pub fn api_key_env_var(&self) -> &'static str {
        match self {
            CliProvider::Claude => "ANTHROPIC_API_KEY",
            CliProvider::Codex => "OPENAI_API_KEY",
            CliProvider::Gemini => "GEMINI_API_KEY",
            CliProvider::Copilot => "GITHUB_TOKEN", // Uses gh OAuth, token optional
        }
    }

    /// Get display name
    pub fn display_name(&self) -> &'static str {
        match self {
            CliProvider::Claude => "Claude Code",
            CliProvider::Codex => "OpenAI Codex",
            CliProvider::Gemini => "Google Gemini",
            CliProvider::Copilot => "GitHub Copilot",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CliProvider::Claude => "claude",
            CliProvider::Codex => "codex",
            CliProvider::Gemini => "gemini",
            CliProvider::Copilot => "copilot",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "codex" | "openai" => CliProvider::Codex,
            "gemini" | "google" => CliProvider::Gemini,
            "copilot" | "github" => CliProvider::Copilot,
            _ => CliProvider::Claude,
        }
    }

    /// Get the flag to skip permission prompts for this CLI
    pub fn skip_permissions_flag(&self) -> &'static str {
        match self {
            CliProvider::Claude => "--dangerously-skip-permissions",
            CliProvider::Codex => "--dangerously-bypass-approvals-and-sandbox",
            CliProvider::Gemini => "-y",
            CliProvider::Copilot => "--yolo",
        }
    }

    /// Look up the matching `Provider` trait object from a built-in registry.
    /// Lets call sites move to the registry-keyed dispatch path incrementally;
    /// new code should prefer `ProviderRegistry` directly.
    pub fn as_provider(&self) -> std::sync::Arc<dyn crate::providers::Provider> {
        crate::providers::ProviderRegistry::built_ins()
            .get(self.as_str())
            .expect("built-in provider registry is missing a known provider id")
    }
}

/// Authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for AuthenticationConfig {
    fn default() -> Self {
        Self {
            cli_provider: CliProvider::default(),
            claude_provider: ClaudeAuthProvider::default(),
            default_model: default_claude_model(),
            github_method: None,
        }
    }
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

    /// Usage analytics configuration.
    #[serde(default)]
    pub usage: UsageConfig,

    /// Plugin enable/disable lists. See [`PluginsConfig`].
    #[serde(default)]
    pub plugins: PluginsConfig,

    /// Where the single `presets.toml` lives. See [`PresetsConfig`].
    #[serde(default)]
    pub presets: PresetsConfig,

    /// Shared MCP pool settings. See [`McpPoolConfig`].
    #[serde(default)]
    pub mcp_pool: McpPoolConfig,
}

/// Shared MCP server pool for host (tmux) sessions.
///
/// When enabled, ainb runs a standalone `ainb mcp daemon` that spawns each
/// shared MCP server ONCE and fronts it with a unix socket under
/// `~/.agents-in-a-box/mcp/sockets/`. Sessions attach via the
/// `ainb mcp proxy <socket>` stdio shim written into each worktree's
/// `.mcp.json`, so N sessions share one server process instead of spawning
/// N node/bun processes.
///
/// Per-server opt-out: set `shared = false` on an `[mcp_servers.<name>]`
/// table to keep that server spawning per-session (use for stateful servers
/// like browser/db bridges).
///
/// Example `config.toml`:
/// ```toml
/// [mcp_pool]
/// enabled = true
/// idle_grace_secs = 300
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpPoolConfig {
    /// Master switch for the shared pool. Off → sessions spawn MCP servers
    /// per-session exactly as before.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Seconds a pooled server child stays alive after its LAST client
    /// detaches before the daemon reaps it. The next attach respawns it.
    #[serde(default = "default_idle_grace_secs")]
    pub idle_grace_secs: u64,
}

impl Default for McpPoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_grace_secs: default_idle_grace_secs(),
        }
    }
}

fn default_idle_grace_secs() -> u64 {
    300
}

/// Where the single `presets.toml` file lives.
///
/// Path is interpreted relative to the config dir
/// (`~/.agents-in-a-box/config/`) when not absolute; `~` is NOT expanded
/// (use absolute paths for non-default locations).
///
/// Defaults to `../presets.toml` so the file sits alongside `config/` at
/// `~/.agents-in-a-box/presets.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresetsConfig {
    /// Path to the presets file, relative to the config dir or absolute.
    /// Defaults to `../presets.toml` (i.e. `~/.agents-in-a-box/presets.toml`).
    #[serde(default = "default_presets_file")]
    pub file: String,
}

impl Default for PresetsConfig {
    fn default() -> Self {
        Self {
            file: default_presets_file(),
        }
    }
}

fn default_presets_file() -> String {
    "../presets.toml".to_string()
}

impl PresetsConfig {
    /// Resolve the configured `file` against the user config directory,
    /// returning the absolute path to the presets file.
    ///
    /// Absolute paths are used verbatim. Relative paths are joined to
    /// `~/.agents-in-a-box/config/` (the user config dir) and canonicalised
    /// lexically (no filesystem touch) so the result is stable even when
    /// the file doesn't exist yet.
    pub fn resolve_path(&self) -> Result<PathBuf> {
        let raw = PathBuf::from(&self.file);
        if raw.is_absolute() {
            return Ok(raw);
        }
        let cfg_dir = AppConfig::get_user_config_dir()?;
        // Join + normalise (`Path::components` collapses `..`).
        let joined = cfg_dir.join(&raw);
        Ok(lexically_normalise(&joined))
    }
}

/// Collapse `.` and `..` components in a path without touching the
/// filesystem. Mirrors Python's `os.path.normpath` semantics — purely
/// syntactic, safe for paths that don't yet exist on disk.
fn lexically_normalise(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Per-plugin filter persisted in `config.toml`.
///
/// Either list is empty by default — meaning "no filter from config".
/// Env vars (`AINB_DISABLE_PLUGINS`, `AINB_DISABLE_PLUGIN`,
/// `AINB_ONLY_PLUGINS`) override these at runtime; see
/// `crates/ainb-core/src/plugins.rs::resolve_plugin_filter` for the
/// precedence ladder.
///
/// Example `config.toml`:
/// ```toml
/// [plugins]
/// disabled = ["burndown"]              # everything except burndown loads
///
/// # OR — allowlist (denylist becomes a no-op when `enabled` is non-empty):
/// [plugins]
/// enabled = ["session-reader"]         # only session-reader loads
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PluginsConfig {
    /// Allowlist — when non-empty, ONLY plugins whose `id` appears here
    /// are loaded. Takes precedence over `disabled` when both are set.
    #[serde(default)]
    pub enabled: Vec<String>,

    /// Denylist — plugins whose `id` appears here are skipped during
    /// discovery. Ignored entirely when `enabled` is non-empty.
    #[serde(default)]
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageConfig {
    #[serde(default)]
    pub plan: Option<UsagePlan>,
    #[serde(default)]
    pub currency: CurrencyConfig,
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            plan: None,
            currency: CurrencyConfig::default(),
            model_aliases: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsagePlan {
    pub id: UsagePlanId,
    pub monthly_usd: f64,
    pub provider: UsagePlanProvider,
    pub reset_day: u8,
    pub set_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UsagePlanId {
    ClaudePro,
    ClaudeMax,
    ClaudeMax5x,
    CursorPro,
    Custom,
    None,
}

impl UsagePlanId {
    pub fn monthly_usd(self) -> Option<f64> {
        match self {
            Self::ClaudePro => Some(20.0),
            Self::ClaudeMax => Some(200.0),
            Self::ClaudeMax5x => Some(100.0),
            Self::CursorPro => Some(20.0),
            Self::Custom | Self::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsagePlanProvider {
    All,
    Claude,
    Codex,
    Cursor,
}

impl Default for UsagePlanProvider {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurrencyConfig {
    #[serde(default = "default_currency_code")]
    pub code: String,
    #[serde(default = "default_currency_symbol")]
    pub symbol: String,
    #[serde(default = "default_exchange_rate")]
    pub usd_rate: f64,
}

impl Default for CurrencyConfig {
    fn default() -> Self {
        Self {
            code: default_currency_code(),
            symbol: default_currency_symbol(),
            usd_rate: default_exchange_rate(),
        }
    }
}

fn default_currency_code() -> String {
    "USD".to_string()
}

fn default_currency_symbol() -> String {
    "$".to_string()
}

fn default_exchange_rate() -> f64 {
    1.0
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

    /// Preferred HomeScreen sidebar width in terminal columns.
    #[serde(default)]
    pub home_sidebar_width: Option<u16>,

    /// Preferred Sessions screen sidebar width in terminal columns.
    #[serde(default)]
    pub sessions_sidebar_width: Option<u16>,

    /// Whether the Sessions screen sidebar starts minimized.
    #[serde(default)]
    pub sessions_sidebar_collapsed: Option<bool>,

    /// User's response to the "wire up Claude Code statusline" prompt.
    /// `Unset` means we'll prompt again (init wizard) and surface the
    /// CTA in the Budget panel. `Declined` suppresses the top-bar CTA
    /// (the Budget-panel CTA remains visible for power users).
    #[serde(default)]
    pub statusline_decision: StatuslineDecision,

    /// User's response to the "install ainb's rich tmux conf" prompt.
    /// Same shape as `statusline_decision`. `Unset` re-prompts on next
    /// `ainb init` if the on-disk conf is Missing or a known-old ainb
    /// default; `Declined` suppresses the prompt entirely.
    #[serde(default)]
    pub tmux_decision: TmuxDecision,
}

/// The user's recorded decision on the Claude Code statusline wiring.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StatuslineDecision {
    /// User has never been asked, or has dismissed the prompt without
    /// accepting or declining.
    #[default]
    Unset,
    /// User explicitly opted out — suppress top-bar CTA.
    Declined,
    /// User accepted; we have written our block.
    Installed,
}

/// The user's recorded decision on the ainb rich tmux conf installation.
/// Mirrors [`StatuslineDecision`] semantics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TmuxDecision {
    /// Never asked, or dismissed without accepting/declining.
    #[default]
    Unset,
    /// Explicitly opted out — suppress future prompts.
    Declined,
    /// Accepted; ainb has written ~/.tmux.conf (and deployed helpers).
    Installed,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            show_container_status: true,
            show_git_status: true,
            preferred_editor: None,
            home_sidebar_width: None,
            sessions_sidebar_width: None,
            sessions_sidebar_collapsed: None,
            statusline_decision: StatuslineDecision::default(),
            tmux_decision: TmuxDecision::default(),
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

                let usage_present = content
                    .parse::<toml::Value>()
                    .ok()
                    .and_then(|value| value.as_table().cloned())
                    .is_some_and(|table| table.contains_key("usage"));

                let file_config: AppConfig = toml::from_str(&content)
                    .with_context(|| format!("Failed to parse config from {}", path.display()))?;

                config.merge_loaded(file_config, usage_present);
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
    pub fn get_config_paths() -> Vec<PathBuf> {
        let mut paths = vec![];

        // 1. Local project config — `.ainb/` is canonical; `.agents-box/`
        //    is the legacy location, still read but listed first so an
        //    `.ainb/` file wins when both exist (later files override).
        if let Ok(cwd) = std::env::current_dir() {
            paths.push(cwd.join(".agents-box").join("config.toml"));
            paths.push(cwd.join(".ainb").join("config.toml"));
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
    pub fn get_user_config_dir() -> Result<PathBuf> {
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        let config_dir = home_dir.join(".agents-in-a-box").join("config");
        Ok(config_dir)
    }

    /// Merge another config into this one
    fn merge(&mut self, other: AppConfig) {
        self.merge_loaded(other, true);
    }

    /// Merge another config into this one, preserving defaulted tables omitted from config files.
    fn merge_loaded(&mut self, other: AppConfig, usage_present: bool) {
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
        if other.ui_preferences.home_sidebar_width.is_some() {
            self.ui_preferences.home_sidebar_width = other.ui_preferences.home_sidebar_width;
        }
        if other.ui_preferences.sessions_sidebar_width.is_some() {
            self.ui_preferences.sessions_sidebar_width =
                other.ui_preferences.sessions_sidebar_width;
        }
        if other.ui_preferences.sessions_sidebar_collapsed.is_some() {
            self.ui_preferences.sessions_sidebar_collapsed =
                other.ui_preferences.sessions_sidebar_collapsed;
        }
        // Decision fields: always trust on-disk, even when the value equals
        // the default. "Unset" is itself a meaningful decision (means: prompt
        // again next time), and "Declined" must round-trip across loads or
        // the wizard would re-pester the user every run.
        self.ui_preferences.statusline_decision = other.ui_preferences.statusline_decision;
        self.ui_preferences.tmux_decision = other.ui_preferences.tmux_decision;

        // Override Docker settings
        if other.docker.host.is_some() {
            self.docker.host = other.docker.host;
        }
        // Only override timeout if it's non-zero (0 indicates old config with unset value)
        if other.docker.timeout != 0 && other.docker.timeout != default_docker_timeout() {
            self.docker.timeout = other.docker.timeout;
        }

        if usage_present {
            self.usage = other.usage;
        }

        // Pool settings: trust the loaded layer whenever it differs from the
        // defaults. `enabled = false` must survive (it IS the default-diverging
        // case); a layer that omits [mcp_pool] deserializes to defaults and
        // changes nothing.
        if other.mcp_pool != McpPoolConfig::default() {
            self.mcp_pool = other.mcp_pool;
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
            usage: UsageConfig::default(),
            plugins: PluginsConfig::default(),
            presets: PresetsConfig::default(),
            mcp_pool: McpPoolConfig::default(),
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
                || k.starts_with("GITHUB_")
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
        config.ui_preferences.home_sidebar_width = Some(42);
        config.ui_preferences.sessions_sidebar_width = Some(44);
        config.ui_preferences.sessions_sidebar_collapsed = Some(true);
        config.usage.plan = Some(UsagePlan {
            id: UsagePlanId::ClaudePro,
            monthly_usd: 20.0,
            provider: UsagePlanProvider::Claude,
            reset_day: 12,
            set_at: "2026-04-29T00:00:00Z".to_string(),
        });
        config.usage.currency = CurrencyConfig {
            code: "GBP".to_string(),
            symbol: "GBP".to_string(),
            usd_rate: 1.0,
        };
        config
            .usage
            .model_aliases
            .insert("cursor-auto".to_string(), "claude-sonnet-4-5".to_string());

        // Serialize to TOML
        let toml_str = toml::to_string_pretty(&config).expect("Failed to serialize config");

        // Verify TOML contains our settings
        assert!(
            toml_str.contains("branch_prefix = \"custom/\""),
            "branch_prefix not in TOML"
        );
        assert!(toml_str.contains("vendor"), "exclude_paths not in TOML");
        assert!(
            toml_str.contains("max_repositories = 1000"),
            "max_repositories not in TOML"
        );
        assert!(
            toml_str.contains("worktree_collision_behavior = \"error\""),
            "worktree_collision_behavior not in TOML"
        );
        assert!(
            toml_str.contains("tcp://localhost:2376"),
            "docker.host not in TOML"
        );
        assert!(
            toml_str.contains("timeout = 120"),
            "docker.timeout not in TOML"
        );
        assert!(toml_str.contains("theme = \"light\""), "theme not in TOML");
        assert!(
            toml_str.contains("show_container_status = false"),
            "show_container_status not in TOML"
        );
        assert!(
            toml_str.contains("show_git_status = false"),
            "show_git_status not in TOML"
        );
        assert!(
            toml_str.contains("preferred_editor = \"nvim\""),
            "preferred_editor not in TOML"
        );
        assert!(
            toml_str.contains("home_sidebar_width = 42"),
            "home_sidebar_width not in TOML"
        );
        assert!(
            toml_str.contains("sessions_sidebar_width = 44"),
            "sessions_sidebar_width not in TOML"
        );
        assert!(
            toml_str.contains("sessions_sidebar_collapsed = true"),
            "sessions_sidebar_collapsed not in TOML"
        );
        assert!(
            toml_str.contains("[usage.plan]") && toml_str.contains("[usage.currency]"),
            "usage config not in TOML"
        );
        assert!(
            toml_str.contains("claude-sonnet-4-5"),
            "model alias not in TOML"
        );

        // Deserialize back
        let loaded: AppConfig = toml::from_str(&toml_str).expect("Failed to deserialize config");

        // Verify all fields match
        assert_eq!(loaded.workspace_defaults.branch_prefix, "custom/");
        assert_eq!(
            loaded.workspace_defaults.exclude_paths,
            vec!["vendor", "dist"]
        );
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
        assert_eq!(
            loaded.ui_preferences.preferred_editor,
            Some("nvim".to_string())
        );
        assert_eq!(loaded.ui_preferences.home_sidebar_width, Some(42));
        assert_eq!(loaded.ui_preferences.sessions_sidebar_width, Some(44));
        assert_eq!(loaded.ui_preferences.sessions_sidebar_collapsed, Some(true));
        assert_eq!(loaded.usage.plan.unwrap().reset_day, 12);
        assert_eq!(loaded.usage.currency.code, "GBP");
        assert_eq!(
            loaded.usage.model_aliases.get("cursor-auto"),
            Some(&"claude-sonnet-4-5".to_string())
        );
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
        assert_eq!(
            base.docker.host,
            Some("unix:///custom/docker.sock".to_string())
        );
        assert_eq!(base.docker.timeout, 90);
    }

    #[test]
    fn usage_built_in_plan_budgets_match_spec() {
        assert_eq!(UsagePlanId::ClaudeMax.monthly_usd(), Some(200.0));
        assert_eq!(UsagePlanId::ClaudeMax5x.monthly_usd(), Some(100.0));
    }

    #[test]
    fn layered_merge_preserves_usage_when_higher_layer_omits_usage() {
        let mut base = AppConfig::default();
        base.usage.plan = Some(UsagePlan {
            id: UsagePlanId::ClaudePro,
            monthly_usd: 20.0,
            provider: UsagePlanProvider::Claude,
            reset_day: 12,
            set_at: "2026-04-29T00:00:00Z".to_string(),
        });
        base.usage.currency = CurrencyConfig {
            code: "GBP".to_string(),
            symbol: "GBP".to_string(),
            usd_rate: 1.0,
        };
        base.usage
            .model_aliases
            .insert("cursor-auto".to_string(), "claude-sonnet-4-5".to_string());

        let mut higher = AppConfig::default();
        higher.ui_preferences.theme = "light".to_string();

        base.merge_loaded(higher, false);

        assert_eq!(base.ui_preferences.theme, "light");
        assert_eq!(base.usage.plan.as_ref().unwrap().reset_day, 12);
        assert_eq!(base.usage.currency.code, "GBP");
        assert_eq!(
            base.usage.model_aliases.get("cursor-auto"),
            Some(&"claude-sonnet-4-5".to_string())
        );
    }

    #[test]
    fn project_config_paths_prefer_ainb_over_legacy() {
        let paths = AppConfig::get_config_paths();
        let ainb = paths.iter().position(|p| p.ends_with(".ainb/config.toml"));
        let legacy = paths.iter().position(|p| p.ends_with(".agents-box/config.toml"));
        let (ainb, legacy) = (ainb.expect(".ainb path missing"), legacy.expect("legacy path missing"));
        // Later files override earlier ones in load(), so `.ainb` must come
        // after `.agents-box` for the canonical location to win.
        assert!(ainb > legacy, "expected .ainb after legacy, got {paths:?}");
    }

    #[test]
    fn mcp_pool_round_trips_through_toml() {
        let mut config = AppConfig::default();
        config.mcp_pool.enabled = false;
        config.mcp_pool.idle_grace_secs = 42;

        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("[mcp_pool]"), "missing section:\n{toml_str}");

        let parsed: AppConfig = toml::from_str(&toml_str).unwrap();
        assert!(!parsed.mcp_pool.enabled);
        assert_eq!(parsed.mcp_pool.idle_grace_secs, 42);
    }

    #[test]
    fn mcp_pool_defaults_when_section_absent() {
        let parsed: AppConfig = toml::from_str("version = \"1.0.0\"").unwrap();
        assert!(parsed.mcp_pool.enabled);
        assert_eq!(parsed.mcp_pool.idle_grace_secs, 300);
    }

    #[test]
    fn layered_merge_respects_mcp_pool_disable() {
        let mut base = AppConfig::default();
        let mut higher = AppConfig::default();
        higher.mcp_pool.enabled = false;

        base.merge_loaded(higher, false);
        assert!(!base.mcp_pool.enabled, "explicit disable must survive merge");

        // A layer that omits [mcp_pool] (== defaults) must not clobber it back.
        base.merge_loaded(AppConfig::default(), false);
        assert!(!base.mcp_pool.enabled, "defaulted layer must not re-enable");
    }

    #[test]
    fn mcp_server_shared_flag_defaults_true_and_round_trips() {
        let toml_str = r#"
            [mcp_servers.ctx]
            name = "ctx"
            description = "d"
            installation = { type = "PreInstalled" }
            definition = { type = "Command", command = "npx", args = ["-y", "pkg"] }
        "#;
        let parsed: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(parsed.mcp_servers["ctx"].shared, "shared defaults to true");

        let toml_str = r#"
            [mcp_servers.browser]
            name = "browser"
            description = "stateful"
            shared = false
            installation = { type = "PreInstalled" }
            definition = { type = "Command", command = "npx", args = [] }
        "#;
        let parsed: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(!parsed.mcp_servers["browser"].shared, "explicit opt-out parses");
    }
}

#[cfg(test)]
mod old_config_tests {
    use super::*;

    #[test]
    fn test_old_config_merge_keeps_default_true_for_booleans() {
        // Start with defaults (which have true for show_container_status and show_git_status)
        let mut defaults = AppConfig::default();
        assert!(
            defaults.ui_preferences.show_container_status,
            "Default should be true"
        );
        assert!(
            defaults.ui_preferences.show_git_status,
            "Default should be true"
        );

        // Simulate an "old config" with empty theme and false values
        let old_config = AppConfig {
            ui_preferences: UiPreferences {
                theme: "".to_string(), // Empty theme indicates old config
                show_container_status: false,
                show_git_status: false,
                preferred_editor: None,
                home_sidebar_width: None,
                sessions_sidebar_width: None,
                sessions_sidebar_collapsed: None,
                statusline_decision: StatuslineDecision::default(),
                tmux_decision: TmuxDecision::default(),
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
                home_sidebar_width: Some(38),
                sessions_sidebar_width: Some(46),
                sessions_sidebar_collapsed: Some(true),
                statusline_decision: StatuslineDecision::default(),
                tmux_decision: TmuxDecision::default(),
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
        assert_eq!(defaults.ui_preferences.home_sidebar_width, Some(38));
        assert_eq!(defaults.ui_preferences.sessions_sidebar_width, Some(46));
        assert_eq!(
            defaults.ui_preferences.sessions_sidebar_collapsed,
            Some(true)
        );

        // Timeout should be updated
        assert_eq!(defaults.docker.timeout, 30);
    }
}
