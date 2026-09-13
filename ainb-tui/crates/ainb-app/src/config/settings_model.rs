// ABOUTME: Value types behind the settings screen and the session-list filter.
// They live beside the config registry that builds them, so the config layer
// does not reach up into the app state machine for its own row types.

/// A section of the settings screen.
///
/// Every [`ConfigRow`](crate::config::ConfigRow) files under one of these, so
/// the list has to cover the whole TOML schema, not just the sections the
/// hand-written rows below happen to reach. The screen renders the subset that
/// actually has rows today; `CONFIG_REGISTRY` is the source of truth for the
/// rest, and wiring it in is what removes that gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigCategory {
    Authentication,
    Workspace,
    Docker,
    AgentDefaults,
    Editor,
    Plugins,
    McpPool,
    Appearance,
    General,
    ContainerTemplates,
    McpServers,
    Fleet,
    Usage,
    Skills,
    SessionReader,
    Presets,
    Daemons,
    Web,
    Acp,
    HangarDaemon,
}

impl ConfigCategory {
    pub fn all() -> Vec<ConfigCategory> {
        vec![
            ConfigCategory::Authentication,
            ConfigCategory::Workspace,
            ConfigCategory::Docker,
            ConfigCategory::AgentDefaults,
            ConfigCategory::Editor,
            ConfigCategory::Plugins,
            ConfigCategory::McpPool,
            ConfigCategory::Appearance,
            ConfigCategory::General,
            ConfigCategory::ContainerTemplates,
            ConfigCategory::McpServers,
            ConfigCategory::Fleet,
            ConfigCategory::Usage,
            ConfigCategory::Skills,
            ConfigCategory::SessionReader,
            ConfigCategory::Presets,
            ConfigCategory::Daemons,
            ConfigCategory::Web,
            ConfigCategory::Acp,
            ConfigCategory::HangarDaemon,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            ConfigCategory::Authentication => "Authentication",
            ConfigCategory::Workspace => "Workspace",
            ConfigCategory::Docker => "Docker",
            ConfigCategory::AgentDefaults => "Agent Defaults",
            ConfigCategory::Editor => "Editor",
            ConfigCategory::Plugins => "Plugins",
            ConfigCategory::McpPool => "MCP Pool",
            ConfigCategory::Appearance => "Appearance",
            ConfigCategory::General => "General",
            ConfigCategory::ContainerTemplates => "Container Templates",
            ConfigCategory::McpServers => "MCP Servers",
            ConfigCategory::Fleet => "Fleet",
            ConfigCategory::Usage => "Usage",
            ConfigCategory::Skills => "Skills",
            ConfigCategory::SessionReader => "Session Reader",
            ConfigCategory::Presets => "Presets",
            ConfigCategory::Daemons => "Daemons",
            ConfigCategory::Web => "Web Dashboard",
            ConfigCategory::Acp => "ACP Adapters",
            ConfigCategory::HangarDaemon => "Hangar Daemon",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            ConfigCategory::Authentication => "🔐",
            ConfigCategory::Workspace => "📁",
            ConfigCategory::Docker => "🐳",
            ConfigCategory::AgentDefaults => "🤖",
            ConfigCategory::Editor => "📝",
            ConfigCategory::Plugins => "🔌",
            ConfigCategory::McpPool => "🧬",
            ConfigCategory::Appearance => "🎨",
            ConfigCategory::General => "⚙️",
            ConfigCategory::ContainerTemplates => "📦",
            ConfigCategory::McpServers => "🛰️",
            ConfigCategory::Fleet => "🚁",
            ConfigCategory::Usage => "💰",
            ConfigCategory::Skills => "🎓",
            ConfigCategory::SessionReader => "📖",
            ConfigCategory::Presets => "🗂️",
            ConfigCategory::Daemons => "🛎️",
            ConfigCategory::Web => "🌐",
            ConfigCategory::Acp => "🔗",
            ConfigCategory::HangarDaemon => "🏗️",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            ConfigCategory::Authentication => "API keys, OAuth, GitHub credentials",
            ConfigCategory::Workspace => "Default paths, git settings, branch prefix",
            ConfigCategory::Docker => "Container host, timeouts",
            ConfigCategory::AgentDefaults => "Model, temperature, max tokens",
            ConfigCategory::Editor => "Preferred code editor for sessions",
            ConfigCategory::Plugins => "Installed plugins, enable/disable",
            ConfigCategory::McpPool => "Shared MCP servers: one process across sessions",
            ConfigCategory::Appearance => "Theme, colors, status indicators",
            ConfigCategory::General => "Default template, presets file",
            ConfigCategory::ContainerTemplates => "Per-template image, resources, mounts",
            ConfigCategory::McpServers => "Per-server install and launch definitions",
            ConfigCategory::Fleet => "Cost caps, interview surface, phone bridge",
            ConfigCategory::Usage => "Plan, currency, model aliases",
            ConfigCategory::Skills => "Catalog release, API key",
            ConfigCategory::SessionReader => "Incremental scan window",
            ConfigCategory::Presets => "Where presets.toml lives",
            ConfigCategory::Daemons => "Staleness windows, notification debounce, approvals",
            ConfigCategory::Web => "`ainb web` bind address and read-only mode",
            ConfigCategory::Acp => "Per-adapter command and pinned permission mode",
            ConfigCategory::HangarDaemon => "Auto-standup and lockdown (stored in the daemon DB)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigSetting {
    pub key: String,
    pub label: String,
    pub value: ConfigValue,
    pub description: String,
}

/// A credential row: the *reference* config.toml stores, plus whether that
/// reference currently resolves to a non-empty secret.
///
/// The screen never renders the plaintext of a credential, and never renders a
/// literal's characters either, only a status and the source it came from. The
/// resolved value is deliberately not kept: nothing on this screen needs it, and
/// not holding it is the cheapest way to guarantee it cannot be painted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretValue {
    /// Exactly what config.toml holds: empty, a literal, `$ENV_VAR`, or
    /// `keychain:<service>`.
    pub reference: String,
    /// Whether `reference` resolved when the row was built. Resolving a
    /// `keychain:` reference shells out to `/usr/bin/security`, so this is
    /// evaluated once at build time and never per frame.
    pub resolved: bool,
}

impl SecretValue {
    /// True when the reference points somewhere else (env var / keychain)
    /// rather than being the secret itself.
    #[must_use]
    pub fn is_reference(&self) -> bool {
        self.reference.starts_with('$') || self.reference.starts_with("keychain:")
    }

    /// `unset` / `resolved (source)` / `unresolved (source)` / `literal …`.
    #[must_use]
    pub fn status_line(&self) -> String {
        if self.reference.trim().is_empty() {
            return "unset".to_string();
        }
        if self.is_reference() {
            let status = if self.resolved {
                "resolved"
            } else {
                "unresolved"
            };
            return format!("{status}  ({})", self.reference);
        }
        // A literal already in the user's config keeps working; we say so
        // rather than rewriting it behind their back.
        "literal  (in config.toml)".to_string()
    }
}

#[derive(Debug, Clone)]
pub enum ConfigValue {
    Text(String),
    /// A credential. Rendered as status + source, never as the value.
    Secret(SecretValue),
    Bool(bool),
    Choice(Vec<String>, usize), // Options and selected index
    Number(i64),
}

impl ConfigValue {
    pub fn display(&self) -> String {
        match self {
            ConfigValue::Text(s) => s.clone(),
            ConfigValue::Secret(secret) => secret.status_line(),
            ConfigValue::Bool(b) => if *b { "✓ Enabled" } else { "✗ Disabled" }.to_string(),
            ConfigValue::Choice(options, idx) => options.get(*idx).cloned().unwrap_or_default(),
            ConfigValue::Number(n) => n.to_string(),
        }
    }

    /// The raw string this widget would persist: the inverse of
    /// [`ConfigRow::to_value`](crate::config::ConfigRow::to_value), and what
    /// [`registry::set_validated`](crate::config::registry::set_validated)
    /// parses back into a typed TOML value.
    pub fn raw(&self) -> String {
        match self {
            ConfigValue::Text(s) => s.clone(),
            ConfigValue::Secret(secret) => secret.reference.clone(),
            ConfigValue::Bool(b) => b.to_string(),
            ConfigValue::Choice(options, idx) => options.get(*idx).cloned().unwrap_or_default(),
            ConfigValue::Number(n) => n.to_string(),
        }
    }
}

/// View filter for the session tree, cycled by `Shift+F` or its clickable title chip.
///
/// Phase 2 of `load_interactive_mode_sessions` started surfacing Stopped sessions
/// (tmux-dead but worktree-alive) alongside Running ones. With many worktrees
/// the tree gets crowded; this filter lets the user hide stopped rows or focus
/// on stopped-only without losing access. Persisted in UI preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionFilter {
    #[default]
    All,
    ActiveOnly,
    StoppedOnly,
}

impl SessionFilter {
    /// Cycle order: All → ActiveOnly → StoppedOnly → All.
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::ActiveOnly,
            Self::ActiveOnly => Self::StoppedOnly,
            Self::StoppedOnly => Self::All,
        }
    }

    /// Short label rendered in the workspace panel title (`[active]` etc.).
    /// Returns None for `All` so the default view stays unmarked.
    pub fn title_label(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::ActiveOnly => Some("active"),
            Self::StoppedOnly => Some("stopped"),
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ActiveOnly => "active",
            Self::StoppedOnly => "stopped",
        }
    }
}
