// ABOUTME: Configuration management for agents-in-a-box
// Handles application config, container defaults, and MCP server definitions

#![allow(dead_code)]

use crate::app::state::SessionFilter;
use crate::audit::{self, AuditResult, AuditTrigger};
use anyhow::{Context, Result};
use dirs;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

pub mod container;
pub mod favorites_store;
pub mod mcp;
pub mod mcp_init;
pub mod onboarding;
pub mod presets;
pub mod registry;
pub mod screen_model;
pub mod session_defaults;
pub mod ssh_display_names;
pub mod tunables;

pub use container::{ContainerTemplate, ContainerTemplateConfig};
pub use favorites_store::{
    DeriveFavoriteError, Favorite, FavoritesStore, MigrationReport,
    SourceType as FavoriteSourceType, favorite_from_local_repo,
};
pub use mcp::{McpInitStrategy, McpInstallation, McpServerConfig, McpServerDefinition};
pub use mcp_init::{McpInitResult, McpInitializer, apply_mcp_init_result};
pub use onboarding::OnboardingConfig;
pub use presets::{PermissionSet, PresetManager, RepositoryPreset, create_default_presets};
pub use registry::{CONFIG_REGISTRY, ConfigRow, Entry as ConfigEntry, RowKind};
pub use session_defaults::{PerRepoDefaults, SessionDefaults};
pub use ssh_display_names::{SessionLabelStore, SshDisplayNameStore, normalize_session_label};
pub use tunables::{
    AcpAdapterConfig, AcpConfig, DaemonsConfig, GeneralConfig, NotifydConfig, UiConfig,
    UsageClientConfig, WebServerConfig,
};

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

    /// Fleet orchestration configuration (budget caps, etc.). See
    /// [`FleetConfig`].
    #[serde(default)]
    pub fleet: FleetConfig,

    /// Shared MCP pool settings. See [`McpPoolConfig`].
    #[serde(default)]
    pub mcp_pool: McpPoolConfig,

    /// Cross-cutting knobs with no other home. See [`GeneralConfig`].
    #[serde(default)]
    pub general: GeneralConfig,

    /// TUI cadences and query bounds. See [`UiConfig`].
    #[serde(default)]
    pub ui: UiConfig,

    /// Daemon-staleness windows. See [`DaemonsConfig`].
    #[serde(default)]
    pub daemons: DaemonsConfig,

    /// How usage data is fetched and cached: core's half of usage, distinct
    /// from the plugin-owned `[usage]`. See [`UsageClientConfig`].
    #[serde(default)]
    pub usage_client: UsageClientConfig,

    /// Notification-daemon knobs, parsed by the daemon itself and mirrored
    /// here so a save round-trips them. See [`NotifydConfig`].
    #[serde(default)]
    pub notifyd: NotifydConfig,

    /// `ainb web` defaults, overridden by its CLI flags. See
    /// [`WebServerConfig`].
    #[serde(default)]
    pub web: WebServerConfig,

    /// The ACP adapter registry. See [`AcpConfig`].
    #[serde(default)]
    pub acp: AcpConfig,
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

    /// Auto-refresh cadence (seconds) for the TUI pool overlay while it's
    /// OPEN. `0` = refresh on open + manual (`r`) only. The overlay never
    /// polls when closed, so this only affects an actively-watched view.
    #[serde(default = "default_monitor_refresh_secs")]
    pub monitor_refresh_secs: u64,

    /// Seconds the daemon itself stays up with ZERO attached clients (across
    /// all servers) before it exits cleanly — removing its sockets and
    /// freeing the process. `ainb run` / import restart it on demand, so this
    /// just stops an unused (or orphaned) pool from lingering forever. `0`
    /// disables self-shutdown (the daemon runs until explicitly stopped).
    #[serde(default = "default_daemon_idle_grace_secs")]
    pub daemon_idle_grace_secs: u64,
}

impl Default for McpPoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_grace_secs: default_idle_grace_secs(),
            monitor_refresh_secs: default_monitor_refresh_secs(),
            daemon_idle_grace_secs: default_daemon_idle_grace_secs(),
        }
    }
}

fn default_idle_grace_secs() -> u64 {
    300
}

fn default_monitor_refresh_secs() -> u64 {
    2
}

fn default_daemon_idle_grace_secs() -> u64 {
    900
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

    /// Per-plugin configuration tables, keyed by plugin name. Each entry is the
    /// serialized `[plugins.<name>]` value table (flat scalars per the plugin's
    /// `[[config]]` schema). The host resolves the entry for a plugin into JSON
    /// and injects it at `plugin/init`. Separate from `enabled`/`disabled`,
    /// which gate *which* plugins load; this carries *how* they're configured.
    ///
    /// `BTreeMap` keeps the serialized order stable so config.toml diffs stay
    /// deterministic across saves.
    #[serde(default, flatten)]
    pub values: BTreeMap<String, toml::Value>,
}

/// Fleet orchestration configuration.
///
/// Currently only carries cost budget caps; reserved as the home for
/// future fleet-wide knobs so they share one `[fleet]` table in
/// `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FleetConfig {
    /// Budget caps for `ainb fleet cost`. See [`CostBudgetConfig`].
    #[serde(default)]
    pub cost: CostBudgetConfig,
    /// Which surface answers Claude interviews. See [`InterviewConfig`].
    #[serde(default)]
    pub interview: InterviewConfig,
    /// Terminal the macOS Fleet app opens when you jump to a session.
    ///
    /// `warp` | `iterm` | `ghostty` | `terminal`. Defaults to `warp`; the macOS
    /// default handler would almost always be Terminal.app, which lands you in
    /// the wrong terminal.
    /// `Option` for the same presence reason as [`InterviewConfig::surface`].
    #[serde(default)]
    pub terminal: Option<String>,
    /// Minutes a session must sit quiet before it reads IDLE.
    ///
    /// One knob for both IDLE producers (the tmux classifier and notifyd's
    /// hook-sourced fold), so a session cannot be idle to one and busy to the
    /// other. Env: `AINB_FLEET_IDLE_MIN`.
    #[serde(default = "default_fleet_idle_min")]
    pub idle_min: u64,

    /// How `ainb fleet send` delivers: `tmux` (send-keys first, broker
    /// fallback), `tmux-only`, or `broker`. Env: `AINB_FLEET_TRANSPORT`.
    #[serde(default = "default_fleet_transport")]
    pub transport: String,

    /// Attach cost/hint enrichment to fleet rows. Off means the reader still
    /// serves free cached suggestions but flags nothing `need_enrich`, so no
    /// producer runs and no tokens are spent. Env: `AINB_FLEET_ENRICH`.
    #[serde(default = "default_true")]
    pub enrich: bool,

    /// Staleness window (ms) for a hook-sourced `current_state` row of a STICKY
    /// kind (ASK/ERR/WAIT/IDLE) before the reader falls back to a live scan.
    /// `0` (the default) disables the check: those kinds stay true until a new
    /// event changes them, so age is a poor staleness signal for them.
    ///
    /// Distinct from [`healthy_state_stale_ms`](Self::healthy_state_stale_ms),
    /// which is the point of splitting them. Both windows used to read the ONE
    /// env var `AINB_FLEET_STATE_STALE_MS` with two different hardcoded
    /// fallbacks (0 here, 300000 there), so setting it moved two unrelated
    /// clocks at once and neither had a single knowable default.
    /// Env: `AINB_FLEET_STATE_STALE_MS`.
    #[serde(default)]
    pub state_stale_ms: i64,

    /// Staleness window (ms) for a hook-sourced row of a HEALTHY-suppressing
    /// kind (RUNNING/DONE). These are the dangerous ones: a daemon that stopped
    /// materializing leaves a stale RUNNING row that suppresses the live scan
    /// forever, so this window has a real floor rather than being off by
    /// default. Env: `AINB_FLEET_HEALTHY_STATE_STALE_MS`, falling back to the
    /// legacy `AINB_FLEET_STATE_STALE_MS` so an existing override keeps working.
    #[serde(default = "default_fleet_healthy_state_stale_ms")]
    pub healthy_state_stale_ms: i64,

    /// Seconds of pane silence after which tmux discovery calls a live session
    /// between turns rather than working. Sized above a slow tool call so a
    /// quiet-but-busy agent is not mislabelled.
    /// Env: `AINB_FLEET_TMUX_IDLE_AFTER_SECS`.
    #[serde(default = "default_fleet_tmux_idle_after_secs")]
    pub tmux_idle_after_secs: i64,

    /// `[fleet.bridge]` carried verbatim, never interpreted here.
    ///
    /// The phone bridge parses this table itself, off the same file, with its
    /// own resolver (`fleet::bridge::config`) — it holds bot tokens, so
    /// `ainb-core` deliberately does not model its shape. It still has to be a
    /// field: `save()` preserves unmodelled *top-level* sections, but `fleet`
    /// IS modelled, so anything nested under it that has no field here would be
    /// dropped on the next save. That is exactly how the bridge's Telegram,
    /// Slack and Discord tokens used to get wiped by any settings save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<toml::Value>,
}

fn default_fleet_terminal() -> &'static str {
    "warp"
}

fn default_fleet_idle_min() -> u64 {
    5
}

fn default_fleet_transport() -> String {
    "tmux".to_string()
}

pub(crate) fn default_fleet_healthy_state_stale_ms() -> i64 {
    5 * 60_000
}

fn default_fleet_tmux_idle_after_secs() -> i64 {
    120
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            cost: CostBudgetConfig::default(),
            interview: InterviewConfig::default(),
            terminal: None,
            idle_min: default_fleet_idle_min(),
            transport: default_fleet_transport(),
            enrich: true,
            state_stale_ms: 0,
            healthy_state_stale_ms: default_fleet_healthy_state_stale_ms(),
            tmux_idle_after_secs: default_fleet_tmux_idle_after_secs(),
            bridge: None,
        }
    }
}

/// Where an `AskUserQuestion` is answered.
///
/// `native` (the DEFAULT) lets Claude draw its own picker immediately and
/// mirrors a read-only card into Fleet. `fleet` makes the PreToolUse hook HOLD
/// the tool call so Fleet or the macOS app can answer it as exact JSON, with no
/// synthetic keystrokes — at the cost of suppressing the terminal picker until
/// someone answers or releases it.
///
/// Native is the default deliberately: holding is the more powerful mode but it
/// strands an operator who is sitting at the terminal, so it is opted into
/// rather than inherited.
///
/// Example `config.toml`:
/// ```toml
/// [fleet.interview]
/// surface = "native"   # or "fleet" to hold for remote answering
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterviewConfig {
    /// `"native"` or `"fleet"`. Unrecognised values read as `"native"`, so a
    /// typo can never silently start holding tool calls.
    ///
    /// `Option` so the merge can tell "this layer said native" from "this layer
    /// said nothing". Comparing against the default cannot: a project config
    /// explicitly setting `native` would be indistinguishable from an absent
    /// section and would lose to a user config saying `fleet`.
    #[serde(default)]
    pub surface: Option<String>,
}

fn default_interview_surface() -> String {
    "native".to_string()
}

impl Default for InterviewConfig {
    fn default() -> Self {
        Self { surface: None }
    }
}

impl FleetConfig {
    /// The effective terminal token, applying the `warp` default.
    #[must_use]
    pub fn terminal_token(&self) -> &str {
        self.terminal.as_deref().unwrap_or(default_fleet_terminal())
    }
}

impl InterviewConfig {
    /// The effective surface token, applying the `native` default.
    #[must_use]
    pub fn surface_token(&self) -> &str {
        self.surface.as_deref().unwrap_or("native")
    }
}

/// Spend ceilings consumed by `ainb fleet cost`.
///
/// A breach fires a notifyd alert (`Notification:budget_exceeded`) for the
/// offending session or group. All thresholds are lifetime USD totals over
/// the reporting window. The blanket `session_usd` / `group_usd` apply to
/// every session / group; the per-key override maps pin a tighter (or
/// looser) ceiling for a named session id or group, taking precedence over
/// the blanket value.
///
/// Example `config.toml`:
/// ```toml
/// [fleet.cost]
/// session_usd = 5.0     # warn when any single session crosses $5
/// group_usd = 25.0      # warn when any workspace group crosses $25
///
/// [fleet.cost.session_overrides]
/// "abc123" = 50.0       # this long-running session gets a $50 ceiling
///
/// [fleet.cost.group_overrides]
/// "infra" = 100.0       # the infra workspace gets a $100 ceiling
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CostBudgetConfig {
    /// Blanket per-session USD ceiling. `None` disables session caps
    /// (except where a `session_overrides` entry sets one explicitly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_usd: Option<f64>,
    /// Blanket per-group USD ceiling. `None` disables group caps (except
    /// where a `group_overrides` entry sets one explicitly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_usd: Option<f64>,
    /// Per-session USD ceilings keyed by session id. Overrides
    /// `session_usd` for the named session.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub session_overrides: HashMap<String, f64>,
    /// Per-group USD ceilings keyed by group name. Overrides `group_usd`
    /// for the named group.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub group_overrides: HashMap<String, f64>,
}

impl CostBudgetConfig {
    /// True when no cap of any kind is configured. The default value is
    /// empty; an empty project layer must not clobber a populated user
    /// layer (see `merge_loaded`).
    pub fn is_empty(&self) -> bool {
        self.session_usd.is_none()
            && self.group_usd.is_none()
            && self.session_overrides.is_empty()
            && self.group_overrides.is_empty()
    }

    /// Resolve the effective USD ceiling for a session id: the override
    /// map wins over the blanket `session_usd`.
    pub fn session_limit(&self, session_id: &str) -> Option<f64> {
        self.session_overrides.get(session_id).copied().or(self.session_usd)
    }

    /// Resolve the effective USD ceiling for a group name: the override
    /// map wins over the blanket `group_usd`.
    pub fn group_limit(&self, group: &str) -> Option<f64> {
        self.group_overrides.get(group).copied().or(self.group_usd)
    }
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

    /// How many directory levels below each scan path the repository scanner
    /// descends. `WorkspaceScanner::with_max_depth` has always existed and was
    /// never wired to anything, so the value was effectively frozen at 3. Deep
    /// trees need more; every extra level multiplies the walk.
    #[serde(default = "default_scan_max_depth")]
    pub scan_max_depth: usize,

    /// Seconds a cached repository scan stays fresh before the next scan walks
    /// the disk again. The cache is also invalidated by a scan path's mtime, so
    /// this is the ceiling on staleness, not the only guard.
    #[serde(default = "default_scan_cache_ttl_secs")]
    pub scan_cache_ttl_secs: i64,
}

impl Default for WorkspaceDefaults {
    fn default() -> Self {
        Self {
            branch_prefix: default_branch_prefix(),
            exclude_paths: Vec::new(),
            workspace_scan_paths: Vec::new(),
            max_repositories: default_max_repositories(),
            worktree_collision_behavior: WorktreeCollisionBehavior::default(),
            scan_max_depth: default_scan_max_depth(),
            scan_cache_ttl_secs: default_scan_cache_ttl_secs(),
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

    /// Whether to show the Sessions-screen bottom keymap legend ("Session
    /// actions" / "Panels & views"). When false it collapses to a single hint
    /// row, reclaiming vertical space for the session list. Toggle with ⇧M.
    #[serde(default = "default_true")]
    pub show_session_menu_bar: bool,

    /// Session-status filter retained across workspace refreshes and restarts.
    #[serde(default)]
    pub session_filter: SessionFilter,

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

    /// Preferred SkillManager screen Sources-panel width in terminal
    /// columns. `None` falls back to the 32-column default. Persisted
    /// on divider-drag / `[`-`]` resize-finish.
    #[serde(default)]
    pub skill_manager_sources_width: Option<u16>,

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

    /// Which nodes of the Settings screen's category tree are expanded,
    /// as `"<category label>|<dotted path>"` ids.
    ///
    /// Persisted UI state rather than a preference — it sits here beside the
    /// sidebar widths for the same reason they do: the screen is unusable if
    /// every section re-collapses on restart, and there is nowhere else that
    /// survives a restart. Registered `Hidden` in the config registry.
    #[serde(default)]
    pub config_tree_expanded: Vec<String>,
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
            show_session_menu_bar: true,
            session_filter: SessionFilter::default(),
            preferred_editor: None,
            home_sidebar_width: None,
            sessions_sidebar_width: None,
            sessions_sidebar_collapsed: None,
            skill_manager_sources_width: None,
            statusline_decision: StatuslineDecision::default(),
            tmux_decision: TmuxDecision::default(),
            config_tree_expanded: Vec::new(),
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

fn default_scan_max_depth() -> usize {
    3
}

fn default_scan_cache_ttl_secs() -> i64 {
    3600
}

/// Merge a higher-layer per-plugin value table into the lower-layer one in
/// place: higher-layer keys win, lower-layer keys the higher layer omits
/// survive. When either side isn't a TOML table (a plugin shipped a scalar
/// under its name), the higher layer replaces wholesale — there's no
/// key-level structure to merge.
fn merge_plugin_value_table(lower: &mut toml::Value, higher: toml::Value) {
    match (lower.as_table_mut(), higher) {
        (Some(lower_table), toml::Value::Table(higher_table)) => {
            for (k, v) in higher_table {
                lower_table.insert(k, v);
            }
        }
        (_, higher) => *lower = higher,
    }
}

/// Sections `ainb-core` reads but must never write back.
///
/// `[usage]` is owned by the burndown plugin, which does its own
/// load-modify-save against this same file. Now that both agree on one path, a
/// long-lived TUI overwriting `[usage]` from the snapshot it loaded at startup
/// would silently revert a plan set from another shell mid-session. Core has no
/// UI for `[usage]` and never assigns to it, so the honest contract is
/// read-only.
const READ_ONLY_SECTIONS: &[&str] = &["usage"];

/// Dotted paths `ainb-core` reads but must never write back from its own
/// snapshot.
///
/// A superset of [`READ_ONLY_SECTIONS`], because the rule is not a top-level
/// one. `[fleet.bridge]` sits INSIDE a section core does model, is carried as an
/// opaque `toml::Value` passthrough, is never assigned by core, and is the table
/// `fleet::bridge::config::SETUP_SKELETON` explicitly tells users to hand-edit.
/// Without this, a settings save writes the startup snapshot back over a bot
/// token edited while the TUI was open — the data loss this whole change exists
/// to stop, one section further in than `[usage]`.
const READ_ONLY_PATHS: &[&str] = &["usage", "fleet.bridge"];

/// Paths `write_keys_into` refuses even for an explicit, key-level edit.
///
/// Narrower than [`READ_ONLY_PATHS`] on purpose. That list stops the *wholesale*
/// overlay clobbering a section from a startup snapshot, which is a staleness
/// problem, not an ownership one. `[fleet.bridge]` needs that protection but is
/// still ours to change when the user actually edits one of its rows — writing
/// the single key they touched is precisely the safe operation. `[usage]` is
/// different: the burndown plugin owns it, so core must not write it at all.
const NEVER_WRITE_PATHS: &[&str] = &["usage"];

/// Write `contents` to `path` atomically, so a crash or a full disk cannot
/// leave a truncated file behind.
///
/// This file now carries three writers' data — the bridge's bot tokens, the
/// skills API key and burndown's `[usage]` — so a partial write is not a
/// cosmetic problem: the next load sees a syntax error and every consumer falls
/// back to defaults. The burndown plugin already writes this way; core matches.
/// A temp filename no other writer can be using.
///
/// The old fixed `config.toml.tmp` is the SAME path `ainb-plugin-burndown`
/// computes for its own atomic save. Now that both write one config.toml, two
/// concurrent saves could each write that file and then rename the other's
/// half-written copy over the real config. Process id plus a random word makes
/// a collision impossible between processes and between threads.
fn temp_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "toml.{}.{:08x}.tmp",
        std::process::id(),
        rand::random::<u32>()
    ))
}

/// Read a config file, mapping ONLY "it is not there" to empty.
///
/// `read_to_string(..).unwrap_or_default()` treats a permission error, an I/O
/// error or a directory in the file's place as an empty file — and the caller
/// then overwrites it, because `rename` needs permission on the DIRECTORY, not
/// the file. `[skills]`, `[session_reader]` and `[fleet.bridge]` all live in
/// that file and would go with it.
pub(crate) fn read_existing(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(anyhow::Error::new(err).context(format!(
            "refusing to save over {} — it exists but could not be read",
            path.display()
        ))),
    }
}

pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    // Follow a symlink to its target before writing. `rename` REPLACES a
    // symlink with a regular file, so a config.toml symlinked into a dotfiles
    // repo would silently stop being the tracked file after the first save,
    // and the mode carry-over below (which follows the link) would give no
    // hint that it had happened.
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let path = resolved.as_path();
    let tmp = temp_path(path);
    // Every failure below removes the temp file. The name is per-process and
    // random, so unlike the old fixed `config.toml.tmp` nothing would ever
    // overwrite a leftover — an aborted write would leave one in the config
    // directory forever.
    if let Err(err) = fs::write(&tmp, contents) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    // Carry the target's permissions onto the replacement. `fs::write` used to
    // truncate in place and keep them; a temp file is created fresh under the
    // umask, so without this a `chmod 600 config.toml` is silently reverted to
    // world-readable on the next save. This file holds bot tokens and an API
    // key, so that is a real downgrade, and the temp file itself sits in the
    // same directory before the rename.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);
        if let Err(err) = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// Write exactly these dotted keys into a config file, leaving every other byte
/// of it alone.
///
/// The narrow counterpart to [`AppConfig::save`], for the cases where
/// serializing the whole struct would be wrong: a key the struct does not model
/// (`[skills]`), or a key whose in-memory snapshot is stale and would revert
/// another writer. Same safety discipline as `save` — refuses a file it cannot
/// read or parse, refuses [`READ_ONLY_SECTIONS`], writes atomically.
///
/// Path-explicit so it is testable without redirecting `HOME`.
/// Remove one dotted key from a config file, preserving everything else.
///
/// The counterpart to [`write_keys_into`] for an optional key being cleared.
/// Storing `""` is not the same as unset: an empty `docker.host` means "connect
/// to nothing" where an absent one means "autodetect", so clearing has to
/// delete the key rather than blank it.
pub(crate) fn remove_key_from(path: &Path, key: &str) -> Result<()> {
    let existing = read_existing(path)?;
    if existing.trim().is_empty() {
        return Ok(());
    }
    let section = key.split('.').next().unwrap_or_default();
    if NEVER_WRITE_PATHS
        .iter()
        .any(|p| *p == section || key.starts_with(&format!("{p}.")))
    {
        anyhow::bail!("'{key}' is in a section ainb-core must not write");
    }
    let mut doc = existing.parse::<toml_edit::DocumentMut>().context(
        "refusing to save over a config.toml that does not parse — fix the syntax error first",
    )?;
    remove_document_key(&mut doc, key);
    write_atomic(path, &doc.to_string())?;
    Ok(())
}

/// Whether `key` lives in a section the burndown plugin owns.
///
/// Core reads `[usage]` but must never write it: the plugin does its own
/// load-modify-save against the same file, so a write from here would revert a
/// plan set from another shell.
#[must_use]
pub(crate) fn is_burndown_owned(key: &str) -> bool {
    let section = key.split('.').next().unwrap_or_default();
    NEVER_WRITE_PATHS
        .iter()
        .any(|p| *p == section || key.starts_with(&format!("{p}.")))
}

pub(crate) fn write_keys_into(path: &Path, edits: &[(String, toml::Value)]) -> Result<()> {
    if edits.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let existing = read_existing(path)?;
    // `toml_edit`, not `toml`: this file is the one users are told to copy from
    // `config/example.config.toml`, which is almost entirely comments. A
    // round-trip through `to_string_pretty` deletes every comment and reflows
    // every line — and this writer runs on a settings edit, not on an explicit
    // "rewrite my config". `config/presets.rs` uses toml_edit for the same
    // reason.
    let mut doc = existing.parse::<toml_edit::DocumentMut>().context(
        "refusing to save over a config.toml that does not parse — fix the syntax error first",
    )?;

    for (key, value) in edits {
        let section = key.split('.').next().unwrap_or_default();
        if NEVER_WRITE_PATHS
            .iter()
            .any(|path| *path == section || key.starts_with(&format!("{path}.")))
        {
            anyhow::bail!("'{key}' is in a section ainb-core must not write");
        }
        set_document_key(&mut doc, key, value)?;
    }

    write_atomic(path, &doc.to_string())?;
    Ok(())
}

/// Every scalar/array leaf in `table`, as dotted paths.
///
/// A leaf is anything that is not a table: writing those individually is what
/// lets a save edit values in place instead of replacing whole sections and
/// flattening them into inline tables.
fn flatten_leaves(
    table: &toml::map::Map<String, toml::Value>,
    prefix: String,
    out: &mut Vec<(String, toml::Value)>,
) {
    for (key, value) in table {
        // Quote a segment that is not a bare TOML key. A model id like
        // `gpt-4.1` splits back into two segments otherwise, inventing a
        // nested table and corrupting the file.
        let segment = registry::quote_key_segment(key.as_ref());
        let path = if prefix.is_empty() {
            segment
        } else {
            format!("{prefix}.{segment}")
        };
        match value {
            // An empty table is a leaf: it still has to be written, or a
            // section that exists only to be present would vanish.
            toml::Value::Table(inner) if !inner.is_empty() => {
                flatten_leaves(inner, path, out);
            }
            other => out.push((path, other.clone())),
        }
    }
}

/// The same walk over a `toml_edit` document, used to find leaves on disk that
/// our snapshot no longer has.
fn flatten_document_leaves(table: &toml_edit::Table, prefix: String, out: &mut Vec<String>) {
    for (key, item) in table.iter() {
        // Quote a segment that is not a bare TOML key. A model id like
        // `gpt-4.1` splits back into two segments otherwise, inventing a
        // nested table and corrupting the file.
        let segment = registry::quote_key_segment(key.as_ref());
        let path = if prefix.is_empty() {
            segment
        } else {
            format!("{prefix}.{segment}")
        };
        // `as_table_like`, not `as_table`: an inline table
        // (`installation = { type = "Npm", … }`, which is exactly how
        // `example.config.toml` documents `[mcp_servers.*]`) is an
        // `Item::Value`, so `as_table()` says no. The `toml` side descends into
        // it either way, so treating it as a leaf here made the prune pass
        // remove it and the set pass re-emit it as `[section.sub]` headers,
        // destroying the line and its comments on the first save.
        match item.as_table_like() {
            Some(inner) if !inner.is_empty() => {
                flatten_document_leaves_like(inner, path, out);
            }
            _ => out.push(path),
        }
    }
}

/// [`flatten_document_leaves`] over anything table-like, including an inline
/// table.
fn flatten_document_leaves_like(
    table: &dyn toml_edit::TableLike,
    prefix: String,
    out: &mut Vec<String>,
) {
    for (key, item) in table.iter() {
        let segment = registry::quote_key_segment(key);
        let path = if prefix.is_empty() {
            segment
        } else {
            format!("{prefix}.{segment}")
        };
        match item.as_table_like() {
            Some(inner) if !inner.is_empty() => {
                flatten_document_leaves_like(inner, path, out);
            }
            _ => out.push(path),
        }
    }
}

/// Remove one dotted key from the document, ignoring a path that is not there.
fn remove_document_key(doc: &mut toml_edit::DocumentMut, key: &str) {
    let parts = registry::parse_dot_key(key);
    let Some((leaf, parents)) = parts.split_last() else {
        return;
    };
    let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for part in parents {
        match table.get_mut(part).and_then(toml_edit::Item::as_table_like_mut) {
            Some(next) => table = next,
            None => return,
        }
    }
    table.remove(leaf);
}

/// Set one dotted key in a `toml_edit` document, creating the parent tables it
/// needs and leaving every other byte of the document untouched.
fn set_document_key(
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    value: &toml::Value,
) -> Result<()> {
    let parts = registry::parse_dot_key(key);
    let Some((leaf, parents)) = parts.split_last() else {
        anyhow::bail!("empty config key");
    };

    let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for part in parents {
        if table.get(part).is_none() {
            let mut created = toml_edit::Table::new();
            // Implicit: a parent that only exists to hold a sub-table does not
            // print an empty `[header]` of its own.
            created.set_implicit(true);
            table.insert(part, toml_edit::Item::Table(created));
        }
        table = table
            .get_mut(part)
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| anyhow::anyhow!("cannot set '{key}': '{part}' is not a table"))?;
    }

    match table.get_mut(leaf) {
        Some(item) => {
            let mut next = to_edit_value(value);
            // Carry the old value's decor across. That decor holds the
            // whitespace AND the trailing `# comment` on the same line, so
            // replacing the item without it silently deletes the note the user
            // wrote next to the setting they just changed.
            if let Some(previous) = item.as_value() {
                *next.decor_mut() = previous.decor().clone();
            }
            *item = toml_edit::Item::Value(next);
        }
        None => {
            table.insert(leaf, toml_edit::Item::Value(to_edit_value(value)));
        }
    }
    Ok(())
}

/// Convert a `toml::Value` into the `toml_edit` value the document stores.
fn to_edit_value(value: &toml::Value) -> toml_edit::Value {
    match value {
        toml::Value::String(s) => s.as_str().into(),
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Float(f) => (*f).into(),
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Datetime(dt) => dt.to_string().into(),
        toml::Value::Array(items) => {
            items.iter().map(to_edit_value).collect::<toml_edit::Array>().into()
        }
        toml::Value::Table(entries) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, entry) in entries {
                inline.insert(key, to_edit_value(entry));
            }
            inline.into()
        }
    }
}

/// The TOML value an external edit writes, or an error naming why the key is
/// not one this writer owns.
///
/// Split out from [`AppConfig::save_external_keys`] so the guard can be tested
/// without resolving `HOME` — a test that calls the writer directly writes to
/// the developer's real config.
fn external_edit_value(key: &str, raw: &str) -> Result<toml::Value> {
    // `fleet.bridge.*` belongs here. Its tokens are parsed by
    // `fleet::bridge::config`, not by serde, and while `[fleet]` IS modelled as
    // an opaque passthrough, `save()` deliberately preserves `fleet.bridge`
    // from disk — so routing a bridge edit through the struct made it a no-op.
    // The key-level write is the one path that actually lands it.
    if !registry::is_external(key) {
        anyhow::bail!("'{key}' is not an externally-owned config key — AppConfig::save owns it");
    }
    let section = key.split('.').next().unwrap_or_default();
    if NEVER_WRITE_PATHS
        .iter()
        .any(|path| *path == section || key.starts_with(&format!("{path}.")))
    {
        anyhow::bail!("'{key}' is in a section ainb-core must not write");
    }
    registry::validate(key, raw)
}

/// Copy the keys of `higher` that `lower` does not already have, descending
/// into tables both sides define. Returns how many leaves were carried across,
/// so the caller can leave the file untouched when the answer is zero.
fn merge_missing_keys(lower: &mut toml::Table, higher: toml::Table) -> usize {
    let mut carried = 0;
    for (key, value) in higher {
        match lower.entry(key) {
            toml::map::Entry::Vacant(slot) => {
                slot.insert(value);
                carried += 1;
            }
            toml::map::Entry::Occupied(mut slot) => {
                // Two tables merge; anything else means the canonical file has
                // a real value here and canonical wins.
                if let (Some(existing), toml::Value::Table(incoming)) =
                    (slot.get_mut().as_table_mut(), value)
                {
                    carried += merge_missing_keys(existing, incoming);
                }
            }
        }
    }
    carried
}

/// Fold a stray `~/.agents-in-a-box/config.toml` into the real user config at
/// `~/.agents-in-a-box/config/config.toml`, then move it aside.
///
/// The burndown and session-reader plugins used to read and write the file one
/// directory up from the one everything else uses, so a user's `[usage]` plan
/// could end up in a file `ainb-core` never opened. Both plugins now agree on
/// the `config/` path; this carries the old file's contents across once, taking
/// the canonical file's value wherever both set the same key.
///
/// Deliberately NOT called from [`AppConfig::load`]: that runs in daemons, in
/// plugin startup and inside the TUI event loop, and a filesystem write as a
/// side effect of a read races every other process doing the same. Call it once
/// from a binary's startup instead — see [`AppConfig::migrate_legacy_paths`].
///
/// Best effort by design: every failure path leaves BOTH files exactly as they
/// were. In particular a canonical file that does not parse aborts the
/// migration rather than being replaced by the stray one — overwriting a config
/// we failed to understand is the data loss this whole change exists to stop.
fn migrate_stray_user_config(canonical: &Path) {
    let Some(stray) = canonical.parent().and_then(Path::parent).map(|d| d.join("config.toml"))
    else {
        return;
    };
    if !stray.exists() || stray == canonical {
        return;
    }

    let Ok(stray_table) = fs::read_to_string(&stray).and_then(|s| {
        s.parse::<toml::Table>()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }) else {
        return;
    };

    // An unreadable or unparseable canonical file must abort the migration.
    // Treating it as empty would write the stray file's contents over a config
    // whose real contents we could not read.
    let mut merged = match fs::read_to_string(canonical) {
        Ok(text) => match text.parse::<toml::Table>() {
            Ok(table) => table,
            Err(err) => {
                tracing::warn!(
                    path = %canonical.display(),
                    %err,
                    "user config does not parse; leaving it alone instead of migrating over it"
                );
                return;
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(err) => {
            tracing::warn!(path = %canonical.display(), %err, "cannot read user config; skipping migration");
            return;
        }
    };

    // Canonical wins: only keys the real config never set are carried over.
    // Recursively, because a top-level-only merge loses everything under a
    // section the canonical file has so much as touched — a canonical
    // `[fleet.cost]` occupies the `fleet` key, and the stray's
    // `[fleet.bridge.telegram] token` is then silently dropped. Losing bridge
    // tokens is the whole reason this migration exists.
    let carried = merge_missing_keys(&mut merged, stray_table);

    // Nothing to carry across: leave the canonical file byte-for-byte alone.
    // Rewriting it would strip the comments and layout of a file users are
    // explicitly pointed at hand-editing.
    if carried > 0 {
        // Through `toml_edit`, like every other writer of this file: the
        // population this migration touches is exactly the people who copied
        // the ~320-line commented `example.config.toml`, and rendering the
        // merged table would delete all of it on first launch, silently.
        let mut doc = match fs::read_to_string(canonical)
            .unwrap_or_default()
            .parse::<toml_edit::DocumentMut>()
        {
            Ok(doc) => doc,
            Err(_) => return,
        };
        let mut leaves = Vec::new();
        flatten_leaves(&merged, String::new(), &mut leaves);
        for (key, value) in &leaves {
            if set_document_key(&mut doc, key, value).is_err() {
                return;
            }
        }
        let rendered = doc.to_string();
        if canonical.parent().is_some_and(|d| fs::create_dir_all(d).is_err()) {
            return;
        }
        if let Err(err) = write_atomic(canonical, &rendered) {
            tracing::warn!(path = %canonical.display(), %err, "failed to write merged user config");
            return;
        }
    }

    // Keep the old file's contents rather than deleting them, and never clobber
    // an earlier backup — in the worst case it is the only surviving copy.
    let mut backup = stray.with_extension("toml.migrated");
    let mut n = 1;
    while backup.exists() {
        backup = stray.with_extension(format!("toml.migrated.{n}"));
        n += 1;
    }
    let _ = fs::rename(&stray, &backup);
    tracing::info!(
        stray = %stray.display(),
        canonical = %canonical.display(),
        backup = %backup.display(),
        carried,
        "merged stray config.toml into the user config"
    );
}
/// Which layer a config file belongs to.
///
/// Carried alongside the path so a caller cannot mislabel it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// `/etc/agents-in-a-box/config.toml` — lowest precedence.
    System,
    /// `~/.agents-in-a-box/config/config.toml`.
    User,
    /// `./.agents-box/config.toml`, the legacy project location.
    ProjectLegacy,
    /// `./.ainb/config.toml` — highest precedence.
    Project,
}

impl ConfigScope {
    /// Human label for `ainb config path`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System config",
            Self::User => "User config",
            Self::ProjectLegacy => "Project config (legacy .agents-box)",
            Self::Project => "Project config",
        }
    }

    /// Stable machine name for `--format json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::ProjectLegacy => "project-legacy",
            Self::Project => "project",
        }
    }
}

impl AppConfig {
    /// One-shot cleanup of config files left behind by older builds.
    ///
    /// Call this ONCE from a binary's startup, before the first
    /// [`AppConfig::load`]. It is deliberately separate from `load()`, which
    /// runs in daemons, plugin startup and the TUI event loop where a
    /// filesystem write as a side effect of a read would race other processes.
    pub fn migrate_legacy_paths() {
        if let Ok(dir) = Self::get_user_config_dir() {
            migrate_stray_user_config(&dir.join("config.toml"));
        }
    }

    /// Load configuration from default locations
    pub fn load() -> Result<Self> {
        // Try loading from multiple locations in order of precedence
        let config_paths = Self::get_config_paths();

        let mut config = Self::default();

        // Load each config file and merge
        for (_scope, path) in config_paths {
            if path.exists() {
                let content = fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read config from {}", path.display()))?;
                config
                    .merge_file_contents(&content)
                    .with_context(|| format!("Failed to parse config from {}", path.display()))?;
            }
        }

        // Load built-in container templates if none exist
        if config.container_templates.is_empty() {
            config.load_builtin_templates();
        }

        Ok(config)
    }

    /// Merge one config file's contents into `self`, exactly as [`load`](Self::load)
    /// does for each path it finds.
    ///
    /// Split out so a test can round-trip a real file through the loader
    /// without depending on `HOME` or the current directory — the field-by-field
    /// `merge_loaded` below is where a newly-added key gets silently dropped,
    /// and only a test that goes through this path catches it.
    pub(crate) fn merge_file_contents(&mut self, content: &str) -> Result<()> {
        let usage_present = content
            .parse::<toml::Value>()
            .ok()
            .and_then(|value| value.as_table().cloned())
            .is_some_and(|table| table.contains_key("usage"));

        let file_config: AppConfig = toml::from_str(content)?;
        self.merge_loaded(file_config, usage_present);
        Ok(())
    }

    /// Overlay the sections this struct models onto whatever is already on
    /// disk, leaving every other top-level section untouched.
    ///
    /// `AppConfig` does not model the whole file. `[skills]` (catalog release +
    /// API key) and `[session_reader]` are parsed straight off the same path by
    /// other crates, and nothing here has a field for them. A plain
    /// `to_string_pretty(self)` write therefore erased them on every settings
    /// save. Overlaying key-by-key keeps them, and keeps any section a future
    /// crate adds, without `ainb-core` needing to know what they are.
    ///
    /// Deliberately shallow: a modelled section (`[fleet]`, `[mcp_servers]`, …)
    /// is replaced wholesale so that *removing* an entry from it actually
    /// sticks. Unmodelled tables nested under a modelled section need a
    /// passthrough field instead — see [`FleetConfig::bridge`].
    fn overlay_onto_existing(&self, existing: &str) -> Result<String> {
        // A file we cannot parse must abort the save, never be treated as
        // empty. `AppConfig::load()` falls back to defaults on a parse error,
        // so defaulting here would let one bad keystroke in a hand-edited file
        // turn the next settings save into a wipe of every section on disk.
        let mut table = if existing.trim().is_empty() {
            toml::Table::new()
        } else {
            let table = existing.parse::<toml::Table>().context(
                "refusing to save over a config.toml that does not parse — fix the syntax error first",
            )?;
            // Second gate, and the one that matters in practice. A file can
            // tokenize perfectly and still fail serde (`timeout = "60"`, a
            // `Vec<String>` holding an int). `AppConfig::load` returns Err on
            // such a file, and several callers `unwrap_or_default()` and then
            // save — so without this, a DEFAULT config gets overlaid and every
            // modelled section on disk is replaced with defaults. Same wipe the
            // syntax gate above exists to stop, one layer down.
            let _: AppConfig = toml::Value::Table(table.clone()).try_into().context(
                "refusing to save over a config.toml that parses but does not load into ainb's \
                 schema — saving would replace every section it models with defaults",
            )?;
            table
        };
        let toml::Value::Table(ours) = toml::Value::try_from(self)? else {
            anyhow::bail!("AppConfig did not serialize to a TOML table");
        };

        // Snapshot the read-only paths BEFORE merging: a modelled section is
        // replaced wholesale, so `[fleet]` takes `[fleet.bridge]` with it and
        // "skip this key" is not enough for a path more than one level deep.
        let on_disk = toml::Value::Table(table.clone());
        let preserved: Vec<(&str, toml::Value)> = READ_ONLY_PATHS
            .iter()
            .filter_map(|path| {
                registry::navigate_toml(&on_disk, path).ok().map(|value| (*path, value.clone()))
            })
            .collect();

        for (key, value) in ours {
            table.insert(key, value);
        }

        let mut root = toml::Value::Table(table);
        for (path, value) in preserved {
            // What is on disk wins for these paths. Absent on disk means core
            // writes its snapshot, which is what `[usage]` already did.
            registry::insert_at(&mut root, path, value)?;
        }

        // Render through `toml_edit`, not `toml::to_string_pretty`, so the
        // file keeps its comments. Users are told to start from
        // `config/example.config.toml`, which is ~320 lines of comments
        // explaining what every key does; a settings save that silently
        // deleted all of them would leave a file strictly less useful than the
        // one it replaced.
        //
        // Setting LEAVES rather than whole sections is what makes that work: a
        // top-level `doc["docker"] = <table>` writes `docker = { timeout = 60 }`
        // as an inline value, flattening the `[docker]` header and taking every
        // comment attached to it. Walking to leaves edits values in place and
        // leaves the document's shape alone.
        let toml::Value::Table(root_table) = root else {
            anyhow::bail!("config did not render to a TOML table");
        };
        let mut doc = existing.parse::<toml_edit::DocumentMut>().unwrap_or_default();

        let mut ours_leaves = Vec::new();
        flatten_leaves(&root_table, String::new(), &mut ours_leaves);

        // Wholesale replacement of a modelled section has to keep working, or
        // removing an entry from `[mcp_servers]` would never stick. Leaf-wise
        // writing cannot express a deletion, so prune first: drop any leaf the
        // document has under a section we model that our snapshot no longer
        // carries. Read-only paths are exempt — they are not ours to prune.
        let ours_keys: std::collections::HashSet<&str> =
            ours_leaves.iter().map(|(k, _)| k.as_str()).collect();
        let mut doc_leaves = Vec::new();
        flatten_document_leaves(doc.as_table(), String::new(), &mut doc_leaves);
        for key in doc_leaves {
            let section = key.split('.').next().unwrap_or_default();
            let is_read_only = READ_ONLY_PATHS
                .iter()
                .any(|path| *path == section || key.starts_with(&format!("{path}.")));
            if is_read_only || !root_table.contains_key(section) {
                continue;
            }
            if !ours_keys.contains(key.as_str()) {
                remove_document_key(&mut doc, &key);
            }
        }

        for (key, value) in &ours_leaves {
            set_document_key(&mut doc, key, value)?;
        }
        Ok(doc.to_string())
    }

    /// Save configuration to user config directory
    pub fn save(&self) -> Result<()> {
        let config_dir = Self::get_user_config_dir()?;
        fs::create_dir_all(&config_dir)?;

        let config_path = config_dir.join("config.toml");
        let existing = read_existing(&config_path)?;
        let content = self.overlay_onto_existing(&existing)?;

        match write_atomic(&config_path, &content) {
            Ok(()) => {
                // The promoted tunables read from a process-wide snapshot, so
                // without this a save reports success and changes nothing until
                // the next launch: syntax highlighting stays on, the inbox keeps
                // its old limit. Re-loads rather than installing `self`, because
                // `load()` also merges the project and system layers.
                tunables::refresh_snapshot();
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

    /// Write registry keys that live in this file but NOT in `AppConfig`'s
    /// serde shape — `[skills]` (owned by `ainb-cli`) and `[session_reader]`
    /// (owned by the session-reader plugin). See
    /// [`EXTERNAL_PREFIXES`](registry::EXTERNAL_PREFIXES).
    ///
    /// [`save`](Self::save) cannot carry these: it serializes `self`, and
    /// `self` has no field for them, so an edit made in the settings screen
    /// would be accepted and then silently dropped — the exact failure mode the
    /// config registry exists to remove. This is a *second* writer over the
    /// same file rather than a way around `save`: it starts from the on-disk
    /// table (so every unmodelled section survives byte-for-byte), refuses a
    /// file that does not parse, validates each value through the registry, and
    /// writes atomically, exactly as `save` does.
    ///
    /// Refuses [`READ_ONLY_SECTIONS`] and any key that is not external, so it
    /// can never become a back door around either guard.
    pub fn save_external_keys(edits: &[(String, String)]) -> Result<()> {
        if edits.is_empty() {
            return Ok(());
        }
        let mut prepared = Vec::with_capacity(edits.len());
        for (key, raw) in edits {
            prepared.push((key.clone(), external_edit_value(key, raw)?));
        }
        let config_path = Self::get_user_config_dir()?.join("config.toml");
        write_keys_into(&config_path, &prepared)
    }

    /// Persist the settings screen's tree expansion, and nothing else.
    ///
    /// Deliberately NOT `save()`: expanding a section is a navigational
    /// keystroke, and `save()` writes the whole in-memory `AppConfig`, which is
    /// the snapshot taken at startup. Navigating would then revert anything
    /// `ainb config set` or another process had changed since — and multiply the
    /// window for two writers to collide.
    pub fn save_tree_expansion(ids: &[String]) -> Result<()> {
        let value =
            toml::Value::Array(ids.iter().map(|id| toml::Value::String(id.clone())).collect());
        let config_path = Self::get_user_config_dir()?.join("config.toml");
        write_keys_into(
            &config_path,
            &[("ui_preferences.config_tree_expanded".to_string(), value)],
        )
    }

    /// Every config file location, in the order [`Self::load`] merges them.
    ///
    /// Each file overrides the last, so the LAST entry wins — which today means
    /// the system config outranks the user's and a project's. That is the
    /// inverse of what the docs and `ainb config path` describe, and it is not
    /// fixed here: `merge_loaded` assigns several fields unconditionally
    /// (`authentication.cli_provider`, `ui_preferences.statusline_decision`,
    /// `workspace_defaults.max_repositories`, …), so "last file wins" currently
    /// means "the last file's serde DEFAULTS win" for those. Reordering without
    /// first making the merge per-field lets a project file containing only
    /// `[docker] timeout = 22` reset a user's provider, wizard decisions and
    /// scan limit — and the next `save()` writes that reset into their file.
    ///
    /// See `agents-in-a-box-l0sq`: the reorder lands with that change,
    /// not before it.
    ///
    /// Returns the scope with each path. They used to be parallel arrays zipped
    /// at the call site, where four paths met three labels and the last one was
    /// silently dropped.
    pub fn get_config_paths() -> Vec<(ConfigScope, PathBuf)> {
        let mut paths = vec![];

        if let Ok(cwd) = std::env::current_dir() {
            paths.push((
                ConfigScope::ProjectLegacy,
                cwd.join(".agents-box").join("config.toml"),
            ));
            paths.push((ConfigScope::Project, cwd.join(".ainb").join("config.toml")));
        }

        if let Ok(config_dir) = Self::get_user_config_dir() {
            paths.push((ConfigScope::User, config_dir.join("config.toml")));
        }

        paths.push((
            ConfigScope::System,
            PathBuf::from("/etc/agents-in-a-box/config.toml"),
        ));

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
        if other.workspace_defaults.scan_max_depth != default_scan_max_depth() {
            self.workspace_defaults.scan_max_depth = other.workspace_defaults.scan_max_depth;
        }
        if other.workspace_defaults.scan_cache_ttl_secs != default_scan_cache_ttl_secs() {
            self.workspace_defaults.scan_cache_ttl_secs =
                other.workspace_defaults.scan_cache_ttl_secs;
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
            self.ui_preferences.show_session_menu_bar = other.ui_preferences.show_session_menu_bar;
            self.ui_preferences.session_filter = other.ui_preferences.session_filter;
        }
        // Old configs keep the default (true) values
        if other.ui_preferences.preferred_editor.is_some() {
            self.ui_preferences.preferred_editor = other.ui_preferences.preferred_editor;
        }
        // A field with no arm here is not merely "not merged": `load()` returns
        // the default for it, and the next `save()` then overlays that default
        // back over the file. An unlisted key is silently destroyed, not just
        // ignored. Covered by `loading_restores_the_config_tree_expansion`.
        if !other.ui_preferences.config_tree_expanded.is_empty() {
            self.ui_preferences.config_tree_expanded = other.ui_preferences.config_tree_expanded;
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
        if other.ui_preferences.skill_manager_sources_width.is_some() {
            self.ui_preferences.skill_manager_sources_width =
                other.ui_preferences.skill_manager_sources_width;
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

        // Fleet cost budgets layer like the plugin tables: a higher layer
        // (project) that declares any cap replaces the lower layer's caps,
        // so a project can pin a tighter spend ceiling without the user
        // default leaking through. An all-empty `[fleet.cost]` (the
        // default) is treated as "not set" and leaves the lower layer
        // intact.
        if !other.fleet.cost.is_empty() {
            self.fleet.cost = other.fleet.cost;
        }
        // `merge_loaded` is a hand-written field-by-field merge, so a struct
        // field that is not named here is silently DROPPED no matter what the
        // file says. An unset section deserializes to the default, so only a
        // non-default value counts as "the file set this".
        if other.fleet.interview.surface.is_some() {
            self.fleet.interview = other.fleet.interview.clone();
        }
        if other.fleet.terminal.is_some() {
            self.fleet.terminal.clone_from(&other.fleet.terminal);
        }
        if other.fleet.bridge.is_some() {
            self.fleet.bridge.clone_from(&other.fleet.bridge);
        }
        // Promoted knobs: a layer counts as having set one only when it differs
        // from the coded default. A field with no arm here is silently DROPPED
        // on load and then overlaid back onto the file by the next save. That
        // is the failure `loading_restores_the_config_tree_expansion` guards.
        if other.fleet.idle_min != default_fleet_idle_min() {
            self.fleet.idle_min = other.fleet.idle_min;
        }
        if other.fleet.transport != default_fleet_transport() {
            self.fleet.transport = other.fleet.transport.clone();
        }
        if !other.fleet.enrich {
            self.fleet.enrich = false;
        }
        if other.fleet.state_stale_ms != 0 {
            self.fleet.state_stale_ms = other.fleet.state_stale_ms;
        }
        if other.fleet.healthy_state_stale_ms != default_fleet_healthy_state_stale_ms() {
            self.fleet.healthy_state_stale_ms = other.fleet.healthy_state_stale_ms;
        }
        if other.fleet.tmux_idle_after_secs != default_fleet_tmux_idle_after_secs() {
            self.fleet.tmux_idle_after_secs = other.fleet.tmux_idle_after_secs;
        }

        // Pool settings: trust the loaded layer whenever it differs from the
        // defaults. `enabled = false` must survive (it IS the default-diverging
        // case); a layer that omits [mcp_pool] deserializes to defaults and
        // changes nothing.
        if other.mcp_pool != McpPoolConfig::default() {
            self.mcp_pool = other.mcp_pool;
        }

        // Plugin enable/disable lists: a higher layer that sets either list
        // replaces the lower layer's (matches the allowlist/denylist intent —
        // the most specific config layer decides which plugins load).
        if !other.plugins.enabled.is_empty() {
            self.plugins.enabled = other.plugins.enabled;
        }
        if !other.plugins.disabled.is_empty() {
            self.plugins.disabled = other.plugins.disabled;
        }
        // Per-plugin config tables layer per key: the higher layer overrides
        // shared keys, but keys it omits keep the lower layer's value (so a
        // project layer can tweak one path without re-declaring the whole
        // table). Mirrors the usage-layering contract.
        for (name, higher_table) in other.plugins.values {
            merge_plugin_value_table(
                self.plugins
                    .values
                    .entry(name)
                    .or_insert_with(|| toml::Value::Table(toml::value::Table::new())),
                higher_table,
            );
        }

        // Promoted sections layer wholesale, like `[mcp_pool]`: a layer that
        // omits the section deserializes to the default and changes nothing, so
        // "differs from the default" is the only usable signal that a file set
        // it. `[acp]` is a map and layers per adapter instead, so a project can
        // repoint one adapter without redeclaring the registry.
        if other.general != GeneralConfig::default() {
            self.general = other.general;
        }
        if other.ui != UiConfig::default() {
            self.ui = other.ui;
        }
        if other.daemons != DaemonsConfig::default() {
            self.daemons = other.daemons;
        }
        if other.usage_client != UsageClientConfig::default() {
            self.usage_client = other.usage_client;
        }
        if other.notifyd != NotifydConfig::default() {
            self.notifyd = other.notifyd;
        }
        if other.web != WebServerConfig::default() {
            self.web = other.web;
        }
        // Per FIELD, not per entry. `HashMap::extend` replaces the whole
        // value, so a higher layer that set only `command` reset
        // `permission_mode` back to the default — which defeats the point of
        // `AcpAdapterToml`'s Option fields, where an absent key means "leave
        // the built-in alone".
        for (name, adapter) in other.acp.adapters {
            let entry = self.acp.adapters.entry(name).or_default();
            if adapter.command.is_some() {
                entry.command = adapter.command;
            }
            // `permission_mode` is a `String` with a serde default here, so an
            // absent key and an explicit `"default"` are indistinguishable
            // after deserialization. Same differs-from-default convention the
            // rest of this merge uses; the cost is that explicitly writing the
            // default in a higher layer cannot override a lower one.
            if adapter.permission_mode != crate::config::tunables::default_acp_permission_mode() {
                entry.permission_mode = adapter.permission_mode;
            }
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
            fleet: FleetConfig::default(),
            mcp_pool: McpPoolConfig::default(),
            general: GeneralConfig::default(),
            ui: UiConfig::default(),
            daemons: DaemonsConfig::default(),
            usage_client: UsageClientConfig::default(),
            notifyd: NotifydConfig::default(),
            web: WebServerConfig::default(),
            acp: AcpConfig::default(),
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

    // ================================================================
    // Review findings #2 #3 #4 #5 #10: what a save is allowed to destroy
    // ================================================================

    /// #2. A file that TOKENIZES but does not load into `AppConfig` must abort
    /// the save.
    ///
    /// `AppConfig::load()` returns Err on such a file, and three callers
    /// (`cli/tmux_install.rs`, `app/state.rs`, `cli/run.rs`) `unwrap_or_default()`
    /// and then save — so a DEFAULT config gets overlaid onto the real file and
    /// every modelled section is replaced with defaults. The syntax gate does
    /// not catch it because the syntax is fine.
    #[test]
    fn saving_over_a_config_that_parses_but_does_not_load_is_refused() {
        // `timeout` is a u64; a quoted number is valid TOML and invalid schema.
        let on_disk = "[docker]\ntimeout = \"60\"\n\n[skills]\napi_key = \"sk-secret\"\n";
        assert!(
            toml::from_str::<AppConfig>(on_disk).is_err(),
            "fixture must be a file that parses but does not load"
        );
        assert!(
            on_disk.parse::<toml::Table>().is_ok(),
            "fixture must tokenize"
        );

        let err = AppConfig::default()
            .overlay_onto_existing(on_disk)
            .expect_err("a config that does not load must not be overlaid");
        assert!(
            err.to_string().contains("does not load"),
            "unexpected error: {err}"
        );
    }

    /// #3. Two writers must not share a temp path. `write_atomic` used a fixed
    /// `config.toml.tmp`, which `ainb-plugin-burndown` computes identically —
    /// so a concurrent burndown save and TUI save could rename each other's
    /// half-written temp over the real config.
    #[test]
    fn atomic_writes_do_not_share_a_temp_path() {
        let path = Path::new("/tmp/config.toml");
        let a = temp_path(path);
        let b = temp_path(path);
        assert_ne!(a, b, "two writes reused one temp path: {a:?}");
        assert!(
            !a.ends_with("config.toml.tmp"),
            "temp path is the fixed name burndown also computes: {a:?}"
        );
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(&std::process::id().to_string()),
            "temp path does not identify the writing process: {a:?}"
        );
    }

    /// #4. Only "not there" may be treated as empty. An unreadable-but-present
    /// file read as `""` is overwritten wholesale, destroying `[skills]`,
    /// `[session_reader]` and `[fleet.bridge]` — the rename needs only
    /// directory permission, so the write succeeds.
    #[test]
    fn an_unreadable_config_is_not_treated_as_empty() {
        let tmp = TempDir::new().expect("tempdir");

        // A directory in the file's place: present, and not readable as text.
        let as_dir = tmp.path().join("config.toml");
        fs::create_dir(&as_dir).expect("mkdir");
        let err = read_existing(&as_dir).expect_err("an unreadable path must not read as empty");
        assert!(err.to_string().contains("could not be read"), "{err}");

        // Absent is still empty.
        assert_eq!(read_existing(&tmp.path().join("absent.toml")).unwrap(), "");
    }

    /// #5. The stray-config migration merged only TOP-LEVEL keys, so a
    /// canonical file with any `[fleet]` content already occupied the `fleet`
    /// key and the stray's `[fleet.bridge.telegram]` tokens were never carried
    /// across — the precise data loss this change exists to stop.
    #[test]
    fn migrate_carries_a_nested_stray_section_into_an_occupied_parent() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join(".agents-in-a-box");
        let canonical = root.join("config").join("config.toml");
        fs::create_dir_all(canonical.parent().unwrap()).expect("mkdir");

        fs::write(&canonical, "[fleet.cost]\nsession_usd = 5.0\n").expect("write canonical");
        fs::write(
            root.join("config.toml"),
            "[fleet.bridge.telegram]\ntoken = \"keychain:ainb-telegram\"\n",
        )
        .expect("write stray");

        migrate_stray_user_config(&canonical);

        let merged: toml::Value =
            toml::from_str(&fs::read_to_string(&canonical).expect("read")).expect("parse");
        assert_eq!(
            registry::navigate_toml(&merged, "fleet.bridge.telegram.token").ok(),
            Some(&toml::Value::String("keychain:ainb-telegram".to_string())),
            "the stray bridge token was dropped because `fleet` was already occupied: {merged}"
        );
        assert_eq!(
            registry::navigate_toml(&merged, "fleet.cost.session_usd").ok(),
            Some(&toml::Value::Float(5.0)),
            "the canonical value must still win"
        );
    }

    // ================================================================
    // Review round 3, findings #1 #2 #3
    // ================================================================

    /// #1. A key `merge_loaded` has no arm for is dropped by every load, and
    /// the next save then writes the empty value back over what was stored.
    ///
    /// Goes through `merge_file_contents`, which is exactly what `load()` runs
    /// per file — a test built on a hand-made struct never touches `merge_loaded`
    /// and so never sees this.
    #[test]
    fn loading_restores_the_config_tree_expansion() {
        let on_disk =
            "[ui_preferences]\nconfig_tree_expanded = [\"Fleet|fleet\", \"MCP Pool|mcp_pool\"]\n";

        let mut config = AppConfig::default();
        config.merge_file_contents(on_disk).expect("loads");

        assert_eq!(
            config.ui_preferences.config_tree_expanded,
            vec!["Fleet|fleet".to_string(), "MCP Pool|mcp_pool".to_string()],
            "the loader dropped the expansion state"
        );

        // And the round trip back to disk must not erase it.
        let written = config.overlay_onto_existing(on_disk).expect("overlay");
        let reread: toml::Value = toml::from_str(&written).expect("parse");
        assert_eq!(
            registry::navigate_toml(&reread, "ui_preferences.config_tree_expanded").ok(),
            Some(&toml::Value::Array(vec![
                toml::Value::String("Fleet|fleet".to_string()),
                toml::Value::String("MCP Pool|mcp_pool".to_string()),
            ])),
            "saving after a load erased the expansion state:\n{written}"
        );
    }

    /// #2. Persisting a setting must not reformat the file.
    ///
    /// `config/example.config.toml` is almost entirely comments and users are
    /// told to copy it. A targeted write that round-trips through
    /// `to_string_pretty` deletes every one of them — on a navigation keypress,
    /// with no save and no confirmation.
    #[test]
    fn a_targeted_write_preserves_comments_and_layout() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let hand_written = "\
# ainb configuration
# Copied from config/example.config.toml — every line below is load-bearing.

[docker]
# Connection timeout in seconds
timeout   = 60

[ui_preferences]
theme = \"dark\"   # keep it dark
";
        fs::write(&path, hand_written).expect("seed");

        write_keys_into(
            &path,
            &[(
                "ui_preferences.config_tree_expanded".to_string(),
                toml::Value::Array(vec![toml::Value::String("Fleet|fleet".to_string())]),
            )],
        )
        .expect("targeted write");

        let after = fs::read_to_string(&path).expect("read");
        for comment in [
            "# ainb configuration",
            "# Copied from config/example.config.toml",
            "# Connection timeout in seconds",
            "# keep it dark",
        ] {
            assert!(
                after.contains(comment),
                "a settings write deleted {comment:?}:\n---\n{after}\n---"
            );
        }
        assert!(
            after.contains("timeout   = 60"),
            "hand-written spacing was reflowed:\n---\n{after}\n---"
        );
        assert!(
            after.contains("config_tree_expanded = [\"Fleet|fleet\"]"),
            "the value was not written:\n---\n{after}\n---"
        );
    }

    /// #3. `[fleet.bridge]` is hand-edited (see `SETUP_SKELETON`) and never
    /// assigned by core, so a settings save must not write it back from the
    /// snapshot it loaded at startup — that reverts a token edited while the
    /// TUI was open. `[usage]` already has this guard; the bridge is the data
    /// this PR exists to protect and had none.
    #[test]
    fn a_save_does_not_revert_a_hand_edited_bridge_token() {
        // What the TUI loaded at startup.
        let mut snapshot = AppConfig::default();
        snapshot.fleet.bridge =
            Some(toml::from_str("[telegram]\ntoken = \"keychain:stale\"\n").expect("stale bridge"));

        // What is on disk now — the user hand-edited the token meanwhile.
        let on_disk =
            "[fleet.bridge.telegram]\ntoken = \"keychain:freshly-edited\"\nuser_id = 42\n";

        let written = snapshot.overlay_onto_existing(on_disk).expect("overlay");
        let after: toml::Value = toml::from_str(&written).expect("parse");

        assert_eq!(
            registry::navigate_toml(&after, "fleet.bridge.telegram.token").ok(),
            Some(&toml::Value::String("keychain:freshly-edited".to_string())),
            "a settings save reverted a hand-edited bridge token:\n{written}"
        );
        assert_eq!(
            registry::navigate_toml(&after, "fleet.bridge.telegram.user_id").ok(),
            Some(&toml::Value::Integer(42)),
            "a settings save dropped a hand-added bridge key:\n{written}"
        );
    }

    /// #9. A navigational keystroke must not rewrite the whole file.
    ///
    /// `ConfigToggleExpand` used to call `AppConfig::save()`, which serializes
    /// the in-memory snapshot taken at startup — so expanding a tree node
    /// reverted anything `ainb config set` or another process had changed since.
    /// The targeted writer touches one key and leaves every other byte.
    #[test]
    fn a_targeted_write_leaves_every_other_section_alone() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        // Stands in for "another writer changed this since we loaded".
        fs::write(
            &path,
            "[docker]\ntimeout = 120\n\n[skills]\napi_key = \"sk-secret\"\n",
        )
        .expect("seed");

        write_keys_into(
            &path,
            &[(
                "ui_preferences.config_tree_expanded".to_string(),
                toml::Value::Array(vec![toml::Value::String("Fleet|fleet".to_string())]),
            )],
        )
        .expect("targeted write");

        let after: toml::Value =
            toml::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(
            registry::navigate_toml(&after, "docker.timeout").ok(),
            Some(&toml::Value::Integer(120)),
            "a tree keystroke reverted another writer's value: {after}"
        );
        assert_eq!(
            registry::navigate_toml(&after, "skills.api_key").ok(),
            Some(&toml::Value::String("sk-secret".to_string()))
        );
        assert_eq!(
            registry::navigate_toml(&after, "ui_preferences.config_tree_expanded").ok(),
            Some(&toml::Value::Array(vec![toml::Value::String(
                "Fleet|fleet".to_string()
            )]))
        );
    }

    /// A targeted write is still refused on a file it cannot parse, and on a
    /// read-only section — the narrow writer is not a way around either guard.
    #[test]
    fn a_targeted_write_keeps_the_same_guards() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        fs::write(&path, "this is not valid toml\n").expect("seed");
        assert!(
            write_keys_into(
                &path,
                &[("docker.timeout".to_string(), toml::Value::Integer(1))]
            )
            .is_err(),
            "wrote over a config.toml that does not parse"
        );

        let ok_path = tmp.path().join("ok.toml");
        fs::write(&ok_path, "[usage]\n").expect("seed");
        assert!(
            write_keys_into(
                &ok_path,
                &[(
                    "usage.currency.code".to_string(),
                    toml::Value::String("EUR".into())
                )]
            )
            .is_err(),
            "wrote into a read-only section"
        );
    }

    /// `fleet.bridge.*` goes THROUGH the key-level writer, and the genuinely
    /// modelled keys stay out of it.
    ///
    /// This assertion used to be the exact opposite. The reasoning was that
    /// `[fleet]` is modelled, so `save()` owns it — but `save()` deliberately
    /// preserves `fleet.bridge` from disk, so routing an edit through the
    /// struct meant it was overwritten by the value already there. The
    /// key-level write is the only path that lands it.
    #[test]
    fn the_external_writer_accepts_bridge_keys_and_refuses_modelled_ones() {
        assert!(
            external_edit_value("fleet.bridge.telegram.token", "keychain:x").is_ok(),
            "a bridge edit must reach the file"
        );
        // The genuinely external sections still go through.
        assert!(external_edit_value("skills.catalog_release", "v1.2.3").is_ok());
        assert!(external_edit_value("session_reader.incremental_window_days", "7").is_ok());
        // A modelled key belongs to `save()`, not here.
        assert!(external_edit_value("docker.timeout", "60").is_err());
    }

    /// A canonical config with a syntax error must abort the migration, not be
    /// replaced by the stray file. Treating an unparseable file as empty would
    /// destroy exactly the sections this change exists to protect.
    #[test]
    fn migrate_leaves_an_unparseable_canonical_config_alone() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join(".agents-in-a-box");
        let canonical = root.join("config").join("config.toml");
        fs::create_dir_all(canonical.parent().unwrap()).expect("mkdir");

        let broken = "[skills]\napi_key = \"sk-secret\"\nthis is not valid toml\n";
        fs::write(&canonical, broken).expect("write canonical");
        fs::write(
            root.join("config.toml"),
            "[usage.currency]\ncode = \"GBP\"\n",
        )
        .expect("write stray");

        migrate_stray_user_config(&canonical);

        assert_eq!(
            fs::read_to_string(&canonical).expect("read"),
            broken,
            "an unparseable canonical config was overwritten by the stray file"
        );
        assert!(
            root.join("config.toml").exists(),
            "the stray file was consumed even though the migration could not run"
        );
    }

    /// A stray file that carries nothing across must leave the canonical file
    /// byte-for-byte alone — users hand-edit it, and a round-trip through the
    /// serializer strips every comment.
    #[test]
    fn migrate_does_not_reformat_when_nothing_is_carried_across() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join(".agents-in-a-box");
        let canonical = root.join("config").join("config.toml");
        fs::create_dir_all(canonical.parent().unwrap()).expect("mkdir");

        let commented = "# my notes\n[ui_preferences]\ntheme = \"dark\"\n";
        fs::write(&canonical, commented).expect("write canonical");
        fs::write(
            root.join("config.toml"),
            "[ui_preferences]\ntheme = \"light\"\n",
        )
        .expect("write stray");

        migrate_stray_user_config(&canonical);

        assert_eq!(
            fs::read_to_string(&canonical).expect("read"),
            commented,
            "the canonical file was reformatted despite nothing being carried across"
        );
        assert!(
            root.join("config.toml.migrated").exists(),
            "stray was not moved aside"
        );
    }

    /// The backup is sometimes the only surviving copy, so a second migration
    /// must not clobber the first one's backup.
    #[test]
    fn migrate_never_overwrites_an_existing_backup() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join(".agents-in-a-box");
        let canonical = root.join("config").join("config.toml");
        fs::create_dir_all(canonical.parent().unwrap()).expect("mkdir");
        fs::write(&canonical, "").expect("write canonical");
        fs::write(root.join("config.toml.migrated"), "first backup\n").expect("write backup");
        fs::write(root.join("config.toml"), "[docker]\ntimeout = 90\n").expect("write stray");

        migrate_stray_user_config(&canonical);

        assert_eq!(
            fs::read_to_string(root.join("config.toml.migrated")).expect("read"),
            "first backup\n",
            "the earlier backup was destroyed"
        );
        assert!(
            root.join("config.toml.migrated.1").exists(),
            "the new backup did not fall back to a free name"
        );
    }

    /// Saving over a file we cannot parse must fail loudly. `AppConfig::load()`
    /// falls back to defaults on a parse error, so silently treating the file as
    /// empty would turn the next settings save into a wipe of everything on disk.
    #[test]
    fn save_refuses_to_overlay_an_unparseable_file() {
        let config = AppConfig::default();
        let err = config
            .overlay_onto_existing("[skills]\napi_key = \"sk\"\nnot valid toml\n")
            .expect_err("overlaying an unparseable file must fail");
        assert!(
            err.to_string().contains("does not parse"),
            "error should say the file is unparseable, got: {err}"
        );
    }

    /// `[usage]` is owned by the burndown plugin, which load-modify-saves the
    /// same file. A long-lived TUI must not revert a plan set from another
    /// shell mid-session.
    #[test]
    fn save_does_not_rewrite_usage_owned_by_the_burndown_plugin() {
        // What the TUI loaded at startup: no plan.
        let config = AppConfig::default();
        // What another process wrote to the file since then.
        let on_disk = "[usage.currency]\ncode = \"GBP\"\nsymbol = \"£\"\nusd_rate = 0.79\n";

        let saved = config.overlay_onto_existing(on_disk).expect("overlays");
        let reparsed: toml::Table = saved.parse().expect("valid TOML");

        assert_eq!(
            reparsed["usage"]["currency"]["code"].as_str(),
            Some("GBP"),
            "a settings save reverted [usage] to the snapshot loaded at startup"
        );
    }

    /// The overlay serializes a `toml::Table`, not the struct, so field
    /// declaration order no longer protects us: TOML requires every top-level
    /// scalar before the first table, and a sorted map interleaves them
    /// (`authentication` sorts before `version`). Prove a fully-populated
    /// config still renders to something that parses back.
    #[test]
    fn overlay_output_is_valid_toml_for_a_full_config() {
        let mut config = AppConfig::default();
        config.load_builtin_templates();

        let rendered = config
            .overlay_onto_existing("[skills]\napi_key = \"sk-secret\"\n")
            .expect("overlay must not fail on a fully-populated config");

        let reparsed: AppConfig = toml::from_str(&rendered).expect("overlay emitted invalid TOML");
        assert_eq!(reparsed.version, config.version);
        assert_eq!(
            reparsed.default_container_template,
            config.default_container_template
        );
        assert_eq!(
            reparsed.container_templates.len(),
            config.container_templates.len()
        );

        let table: toml::Table = rendered.parse().expect("valid TOML");
        assert_eq!(table["skills"]["api_key"].as_str(), Some("sk-secret"));
    }

    /// A user who set `[usage]` while the plugins still read the file one
    /// directory up must not lose it. The stray file is folded in and moved
    /// aside, and the canonical file wins wherever both set the same key.
    #[test]
    fn migrate_folds_stray_config_in_and_moves_it_aside() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join(".agents-in-a-box");
        let canonical = root.join("config").join("config.toml");
        fs::create_dir_all(canonical.parent().unwrap()).expect("mkdir");

        fs::write(
            root.join("config.toml"),
            "[usage.currency]\ncode = \"GBP\"\n\n[ui_preferences]\ntheme = \"light\"\n",
        )
        .expect("write stray");
        fs::write(&canonical, "[ui_preferences]\ntheme = \"dark\"\n").expect("write canonical");

        migrate_stray_user_config(&canonical);

        let merged: toml::Table =
            fs::read_to_string(&canonical).expect("read").parse().expect("valid TOML");
        assert_eq!(
            merged["usage"]["currency"]["code"].as_str(),
            Some("GBP"),
            "the stray file's [usage] was not carried across"
        );
        assert_eq!(
            merged["ui_preferences"]["theme"].as_str(),
            Some("dark"),
            "the stray file overwrote a key the canonical config already set"
        );
        assert!(
            !root.join("config.toml").exists(),
            "stray file was left in place"
        );
        assert!(
            root.join("config.toml.migrated").exists(),
            "stray file's contents were destroyed rather than kept"
        );
    }

    /// A `fleet.bridge.*` edit must actually reach the file.
    ///
    /// This is the row class the whole PR exists to protect, and it was broken
    /// twice in a row: first routed through the struct, where `save()`'s
    /// preservation of `fleet.bridge` silently overwrote it, then routed to the
    /// key-level writer which still refused the prefix. Both times the screen
    /// reported "Setting saved to config.toml" and nothing changed.
    #[test]
    fn a_bridge_edit_is_accepted_by_the_external_writer() {
        let value = external_edit_value("fleet.bridge.telegram.token", "keychain:ainb-telegram")
            .expect("a bridge edit must be an accepted external write");
        assert_eq!(value.as_str(), Some("keychain:ainb-telegram"));
    }

    /// The counterpart: `[usage]` belongs to the burndown plugin, and core must
    /// keep refusing it.
    #[test]
    fn a_usage_edit_is_refused_by_the_external_writer() {
        // Refused at the `is_external` gate, before the ownership check even
        // runs — `[usage]` is not an externally-owned section, it is a modelled
        // one core reads and the burndown plugin writes.
        assert!(
            external_edit_value("usage.currency.code", "GBP").is_err(),
            "core must not write a section the burndown plugin owns"
        );
    }

    /// And it must be refused with something actionable, not a bare failure
    /// three calls downstream.
    #[test]
    fn burndown_owned_keys_are_recognised() {
        assert!(is_burndown_owned("usage.currency.code"));
        assert!(is_burndown_owned("usage"));
        assert!(!is_burndown_owned("fleet.bridge.telegram.token"));
        assert!(!is_burndown_owned("docker.timeout"));
    }

    /// An inline table must survive a save intact.
    ///
    /// `example.config.toml` documents `[mcp_servers.*]` with inline
    /// `installation = { type = "Npm", … }` / `definition = { … }` values. An
    /// inline table is an `Item::Value`, so `as_table()` says no: the document
    /// walk called it a leaf while the `toml` walk descended into it, the
    /// prune pass then removed it and the set pass re-emitted it as
    /// `[section.sub]` headers — destroying the line and its comment.
    #[test]
    fn save_keeps_an_inline_table_and_its_comment() {
        let existing = "\
# my mcp servers
[mcp_servers.context7]
name = \"context7\"
description = \"docs\"
required_env = []
enabled_by_default = true
shared = true
installation = { type = \"Npm\", package = \"@context7/mcp-server\", version = \"latest\" }  # inline
definition = { type = \"Command\", command = \"npx\", args = [\"-y\"], env = {} }

[docker]
timeout = 60
";
        let config: AppConfig = toml::from_str(existing).expect("parses");
        let saved = config.overlay_onto_existing(existing).expect("overlays");

        assert!(
            saved.contains("installation = {"),
            "the inline table was expanded into headers:\n{saved}"
        );
        assert!(
            saved.contains("# inline"),
            "the inline comment was lost:\n{saved}"
        );
        assert!(
            saved.contains("# my mcp servers"),
            "the section comment was lost:\n{saved}"
        );
        assert!(
            !saved.contains("[mcp_servers.context7.installation]"),
            "the inline table was re-emitted as a header:\n{saved}"
        );
        toml::from_str::<AppConfig>(&saved).expect("saved config no longer deserializes");
    }

    /// A map key containing a dot must survive a save.
    ///
    /// Model ids routinely have dots, and `[usage.model_aliases]` is keyed by
    /// them. Flattening `gpt-4.1` into a dotted path and splitting it again
    /// invented a `[usage.model_aliases.gpt-4]` table with a `1` inside,
    /// alongside the untouched original — after which the file no longer
    /// deserialized and every consumer silently fell back to defaults.
    #[test]
    fn save_keeps_a_map_key_that_contains_a_dot() {
        let existing = "\
[usage.model_aliases]
\"gpt-4.1\" = \"claude-sonnet-4-5\"

[docker]
timeout = 60
";
        let config: AppConfig = toml::from_str(existing).expect("parses");
        let saved = config.overlay_onto_existing(existing).expect("overlays");

        let reparsed: toml::Table = saved.parse().expect("still valid TOML");
        assert_eq!(
            reparsed["usage"]["model_aliases"]["gpt-4.1"].as_str(),
            Some("claude-sonnet-4-5"),
            "the dotted map key did not survive the save:\n{saved}"
        );
        assert!(
            reparsed["usage"]["model_aliases"].get("gpt-4").is_none(),
            "the dotted key was split into a nested table:\n{saved}"
        );
        // The whole point: it must still load.
        toml::from_str::<AppConfig>(&saved).expect("saved config no longer deserializes");
    }

    /// A settings save must not strip the file's comments.
    ///
    /// Users are told to start from `config/example.config.toml`, which is
    /// ~320 lines of comments explaining what every key does. Rendering the
    /// save through `toml::to_string_pretty` deleted all of them, so the first
    /// time anyone changed a setting the file they were left with was strictly
    /// less useful than the one they copied.
    #[test]
    fn save_keeps_the_comments_in_a_hand_written_config() {
        let existing = "\
# The theme the TUI paints with.
# Options: \"dark\" | \"light\"
[ui_preferences]
theme = \"dark\"   # trailing note

# How long to wait on the Docker daemon.
[docker]
timeout = 60
";
        let mut config: AppConfig = toml::from_str(existing).expect("parses");
        config.ui_preferences.theme = "light".to_string();

        let saved = config.overlay_onto_existing(existing).expect("overlays");

        assert!(
            saved.contains("# The theme the TUI paints with."),
            "a leading comment was stripped by save:\n{saved}"
        );
        assert!(
            saved.contains("# How long to wait on the Docker daemon."),
            "a section comment was stripped by save:\n{saved}"
        );
        assert!(
            saved.contains("# trailing note"),
            "a trailing comment was stripped by save:\n{saved}"
        );
        assert!(
            saved.contains("theme = \"light\""),
            "the edited value did not reach the file:\n{saved}"
        );
    }

    /// Saving must not eat the sections `AppConfig` does not model.
    ///
    /// `[skills]` and `[session_reader]` are read off this same file by
    /// `ainb-cli` and the session-reader plugin; `[fleet.bridge]` holds the
    /// phone bridge's bot tokens. Before the overlay write, every settings save
    /// replaced the file wholesale and silently destroyed all three.
    #[test]
    fn save_preserves_sections_app_config_does_not_model() {
        let existing = r#"
[skills]
catalog_release = "v1.5.0"
api_key = "sk-secret"

[session_reader]
incremental_window_days = 90

[fleet.bridge.telegram]
token = "keychain:ainb-telegram"
user_id = 123456789

[ui_preferences]
theme = "dark"
"#;
        // Round-trip through load so `[fleet.bridge]` reaches the struct the
        // way a real save does, then write back over the same content.
        let loaded: AppConfig = toml::from_str(existing).expect("parses");
        let saved = loaded.overlay_onto_existing(existing).expect("overlays");
        let reparsed: toml::Table = saved.parse().expect("still valid TOML");

        assert_eq!(
            reparsed["skills"]["catalog_release"].as_str(),
            Some("v1.5.0"),
            "[skills] was dropped by save"
        );
        assert_eq!(
            reparsed["skills"]["api_key"].as_str(),
            Some("sk-secret"),
            "the skills API key was dropped by save"
        );
        assert_eq!(
            reparsed["session_reader"]["incremental_window_days"].as_integer(),
            Some(90),
            "[session_reader] was dropped by save"
        );
        assert_eq!(
            reparsed["fleet"]["bridge"]["telegram"]["token"].as_str(),
            Some("keychain:ainb-telegram"),
            "[fleet.bridge] was dropped by save — bridge tokens are gone"
        );
    }

    /// The flip side of the overlay: a modelled section is replaced wholesale,
    /// so deleting an entry from it actually sticks instead of being resurrected
    /// from the previous file contents.
    #[test]
    fn save_replaces_modelled_sections_wholesale() {
        let existing = r#"
[ui_preferences]
theme = "light"
show_git_status = false
"#;
        let mut config: AppConfig = toml::from_str(existing).expect("parses");
        config.ui_preferences.show_git_status = true;

        let saved = config.overlay_onto_existing(existing).expect("overlays");
        let reparsed: toml::Table = saved.parse().expect("still valid TOML");

        assert_eq!(
            reparsed["ui_preferences"]["show_git_status"].as_bool(),
            Some(true),
            "a modelled field kept its stale on-disk value"
        );
    }

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
        let ainb = paths.iter().position(|(_, p)| p.ends_with(".ainb/config.toml"));
        let legacy = paths.iter().position(|(_, p)| p.ends_with(".agents-box/config.toml"));
        let (ainb, legacy) = (
            ainb.expect(".ainb path missing"),
            legacy.expect("legacy path missing"),
        );
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
        assert!(
            toml_str.contains("[mcp_pool]"),
            "missing section:\n{toml_str}"
        );

        let parsed: AppConfig = toml::from_str(&toml_str).unwrap();
        assert!(!parsed.mcp_pool.enabled);
        assert_eq!(parsed.mcp_pool.idle_grace_secs, 42);
    }

    #[test]
    fn mcp_pool_defaults_when_section_absent() {
        let parsed: AppConfig = toml::from_str("version = \"1.0.0\"").unwrap();
        assert!(parsed.mcp_pool.enabled);
        assert_eq!(parsed.mcp_pool.idle_grace_secs, 300);
        // Daemon self-shutdown is on by default (15 min idle) so an unused or
        // orphaned pool can't linger forever; 0 would disable it.
        assert_eq!(parsed.mcp_pool.daemon_idle_grace_secs, 900);
    }

    #[test]
    fn mcp_pool_daemon_idle_grace_round_trips() {
        let mut config = AppConfig::default();
        config.mcp_pool.daemon_idle_grace_secs = 0; // disable self-shutdown
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: AppConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mcp_pool.daemon_idle_grace_secs, 0);
    }

    #[test]
    fn layered_merge_respects_mcp_pool_disable() {
        let mut base = AppConfig::default();
        let mut higher = AppConfig::default();
        higher.mcp_pool.enabled = false;

        base.merge_loaded(higher, false);
        assert!(
            !base.mcp_pool.enabled,
            "explicit disable must survive merge"
        );

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
        assert!(
            !parsed.mcp_servers["browser"].shared,
            "explicit opt-out parses"
        );
    }

    #[test]
    fn test_plugins_values_roundtrip() {
        // config.toml with a `[plugins.learnings]` value table parses into
        // `PluginsConfig.values["learnings"]`, survives a save()→reload round
        // trip, leaves the existing enabled/disabled lists untouched, and an
        // absent `[plugins.<x>]` yields an empty map (serde default).
        let toml_src = r#"
[plugins]
disabled = ["burndown"]

[plugins.learnings]
learnings_dir = "x"
qmd_collection = "learnings"
"#;
        let cfg: AppConfig = toml::from_str(toml_src).expect("parse plugins.values");

        // The nested table lands under values, keyed by plugin name.
        let learnings = cfg.plugins.values.get("learnings").expect("learnings value table present");
        assert_eq!(
            learnings.get("learnings_dir").and_then(toml::Value::as_str),
            Some("x")
        );
        assert_eq!(
            learnings.get("qmd_collection").and_then(toml::Value::as_str),
            Some("learnings")
        );
        // Existing enable/disable lists are unaffected by the new field.
        assert_eq!(cfg.plugins.disabled, vec!["burndown".to_string()]);
        assert!(cfg.plugins.enabled.is_empty());

        // save()→reload identity: serialize, parse back, compare the values map.
        let serialized = toml::to_string_pretty(&cfg).expect("serialize");
        let reloaded: AppConfig = toml::from_str(&serialized).expect("reparse");
        assert_eq!(reloaded.plugins.values, cfg.plugins.values);

        // Absent `[plugins.<x>]` → empty map via serde default.
        let bare: AppConfig = toml::from_str("[plugins]\n").expect("bare plugins");
        assert!(bare.plugins.values.is_empty(), "absent table → empty map");
    }

    #[test]
    fn test_fleet_cost_budget_roundtrip_and_resolution() {
        let toml_src = r#"
[fleet.cost]
session_usd = 5.0
group_usd = 25.0

[fleet.cost.session_overrides]
"abc123" = 50.0

[fleet.cost.group_overrides]
"infra" = 100.0
"#;
        let cfg: AppConfig = toml::from_str(toml_src).expect("parse fleet.cost");
        let cost = &cfg.fleet.cost;
        assert_eq!(cost.session_usd, Some(5.0));
        assert_eq!(cost.group_usd, Some(25.0));

        // Overrides win over the blanket caps; everything else falls back.
        assert_eq!(cost.session_limit("abc123"), Some(50.0));
        assert_eq!(cost.session_limit("other"), Some(5.0));
        assert_eq!(cost.group_limit("infra"), Some(100.0));
        assert_eq!(cost.group_limit("other"), Some(25.0));

        // save()→reload identity.
        let serialized = toml::to_string_pretty(&cfg).expect("serialize");
        let reloaded: AppConfig = toml::from_str(&serialized).expect("reparse");
        assert_eq!(reloaded.fleet.cost, cfg.fleet.cost);

        // Absent `[fleet.cost]` → empty (no caps).
        let bare: AppConfig = toml::from_str("[fleet]\n").expect("bare fleet");
        assert!(bare.fleet.cost.is_empty());
        assert_eq!(bare.fleet.cost.session_limit("x"), None);
    }

    #[test]
    fn test_fleet_cost_project_layer_overrides_user_layer() {
        // User layer sets a $5 session cap; an empty project `[fleet.cost]`
        // must NOT clobber it, but a populated one must replace it.
        let mut user: AppConfig = toml::from_str("[fleet.cost]\nsession_usd = 5.0\n").unwrap();

        // Empty project fleet config → user cap preserved.
        let empty_project: AppConfig = toml::from_str("[fleet]\n").unwrap();
        let mut merged = user.clone();
        merged.merge(empty_project);
        assert_eq!(merged.fleet.cost.session_usd, Some(5.0));

        // Populated project config → replaces the user cap.
        let project: AppConfig = toml::from_str("[fleet.cost]\nsession_usd = 2.0\n").unwrap();
        user.merge(project);
        assert_eq!(user.fleet.cost.session_usd, Some(2.0));
    }

    #[test]
    fn test_plugins_values_layering() {
        // A higher (project) layer overrides the lower (user) layer for the
        // same `[plugins.<n>].<key>`, mirroring the usage-layering contract.
        let mut base = AppConfig::default();
        let mut user_table = toml::value::Table::new();
        user_table.insert("learnings_dir".into(), toml::Value::String("user".into()));
        user_table.insert(
            "qmd_collection".into(),
            toml::Value::String("base-only".into()),
        );
        base.plugins.values.insert("learnings".into(), toml::Value::Table(user_table));

        let mut higher = AppConfig::default();
        let mut project_table = toml::value::Table::new();
        project_table.insert(
            "learnings_dir".into(),
            toml::Value::String("project".into()),
        );
        higher
            .plugins
            .values
            .insert("learnings".into(), toml::Value::Table(project_table));

        base.merge_loaded(higher, false);

        let merged = base
            .plugins
            .values
            .get("learnings")
            .and_then(toml::Value::as_table)
            .expect("merged learnings table");
        // Higher layer wins for the shared key.
        assert_eq!(
            merged.get("learnings_dir").and_then(toml::Value::as_str),
            Some("project")
        );
        // Keys only present in the lower layer survive the merge.
        assert_eq!(
            merged.get("qmd_collection").and_then(toml::Value::as_str),
            Some("base-only")
        );
    }
}

#[cfg(test)]
mod old_config_tests {
    use super::*;

    #[test]
    fn session_filter_preference_round_trips() {
        let preferences = UiPreferences {
            session_filter: SessionFilter::ActiveOnly,
            ..UiPreferences::default()
        };
        let decoded: UiPreferences =
            toml::from_str(&toml::to_string(&preferences).unwrap()).unwrap();
        assert_eq!(decoded.session_filter, SessionFilter::ActiveOnly);
    }

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
                show_session_menu_bar: false,
                session_filter: SessionFilter::default(),
                preferred_editor: None,
                home_sidebar_width: None,
                sessions_sidebar_width: None,
                sessions_sidebar_collapsed: None,
                skill_manager_sources_width: None,
                statusline_decision: StatuslineDecision::default(),
                tmux_decision: TmuxDecision::default(),
                config_tree_expanded: Vec::new(),
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
                show_session_menu_bar: false,
                session_filter: SessionFilter::default(),
                preferred_editor: None,
                home_sidebar_width: Some(38),
                sessions_sidebar_width: Some(46),
                sessions_sidebar_collapsed: Some(true),
                skill_manager_sources_width: None,
                statusline_decision: StatuslineDecision::default(),
                tmux_decision: TmuxDecision::default(),
                config_tree_expanded: Vec::new(),
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
