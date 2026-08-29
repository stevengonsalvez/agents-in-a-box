// ABOUTME: One table describing every user-facing configuration leaf.
// Pairs each dotted TOML path with its widget, help text and validation, so the
// settings screen and `ainb config set` stop disagreeing about what exists.

//! The configuration registry.
//!
//! The settings screen used to carry a hand-written list of rows. Nothing tied
//! that list to the actual serde schema, so the two drifted: the schema grew to
//! ~95 leaves while the screen reached 12 of them, and ten rows edited state
//! that no persist branch ever read back.
//!
//! This module is the single place a leaf is declared. Every path in the
//! serialized [`AppConfig`](crate::config::AppConfig) is either a
//! [`Entry::Row`] (a real preference, with a label, one line of help and a
//! [`RowKind`] that drives both the widget and the validator) or an
//! [`Entry::Hidden`] (persisted UI/wizard state that is not a preference, with
//! a written reason). `every_leaf_has_an_entry` in this file's test module
//! fails the build when a new schema field has neither, so "a TOML key exists"
//! now implies "a menu row exists".
//!
//! The shape deliberately mirrors the plugin `[[config]]` contract
//! (`ainb_plugin_protocol::manifest::ConfigField`) that already round-trips
//! plugin settings end to end. It is a core-local type rather than a reuse
//! because rows need help text and numeric ranges, and because the plugin wire
//! surface is frozen by a conformance gate that a core-side need should never
//! force a version bump on.
//!
//! ## Map keys
//!
//! `[container_templates.<name>]`, `[mcp_servers.<name>]` and
//! `[plugins.<name>]` are keyed by user-chosen names. Those segments normalise
//! to a literal `*` in registry keys ("mcp_servers.*.shared"), so one entry
//! covers every instance. [`registry_key`] performs that normalisation for a
//! concrete path.

use anyhow::{Result, anyhow, bail};

use crate::app::state::{ConfigCategory, ConfigSetting, ConfigValue};

/// The value type of a configuration leaf: selects the widget the settings
/// screen renders, and the validator `ainb config set` applies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowKind {
    /// Free-form string (text input).
    Text,
    /// A credential. Rendered masked; the value in config.toml may be a
    /// literal, an env ref (`$VAR`) or a keychain ref (`keychain:<service>`).
    Secret,
    /// Boolean toggle.
    Bool,
    /// Integer within an inclusive range.
    Number { min: i64, max: i64 },
    /// Floating point within an inclusive range (money, rates, CPU shares).
    Float { min: f64, max: f64 },
    /// One of an enumerated set. The strings are the *serialized* forms, not
    /// display names, so they can be written straight back into config.toml.
    Choice(&'static [&'static str]),
    /// Array of scalars, edited as a comma-separated list. The element type is
    /// explicit because it cannot be guessed: `definition.args` is a
    /// `Vec<String>` whose elements often look like numbers (`"8931"`), while
    /// `config.ports` is a `Vec<u16>` that must NOT be stored as strings.
    List(ListElement),
    /// A structured value (array of tables, or a nested JSON blob) with no
    /// scalar rendering. Displayed read-only; `ainb config set` refuses it and
    /// points at `ainb config edit`.
    Opaque,
}

/// What a [`RowKind::List`] holds. The list widget edits a comma-separated
/// string either way; this decides what each element parses back into.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ListElement {
    /// Elements stay strings, however numeric they look.
    Text,
    /// Elements are whole numbers (ports, ids).
    Integer,
}

/// One editable configuration leaf.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfigRow {
    /// Dotted TOML path, with map keys normalised to `*`.
    pub key: &'static str,
    /// Which settings category the row files under.
    pub category: ConfigCategory,
    /// Short human label.
    pub label: &'static str,
    /// One line of help; becomes [`ConfigSetting::description`].
    pub help: &'static str,
    /// Value type: widget + validation.
    pub kind: RowKind,
}

/// A registry entry: either a user-facing row or a deliberately hidden leaf.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Entry {
    /// A real preference. Rendered, editable, validated.
    Row(ConfigRow),
    /// Internal state, not a user preference. `why` is documentation for the
    /// next reader and is asserted non-empty by the tests, because an unexplained
    /// hidden leaf is how a real setting quietly disappears.
    Hidden {
        key: &'static str,
        why: &'static str,
    },
}

impl Entry {
    /// The dotted key this entry claims.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Entry::Row(row) => row.key,
            Entry::Hidden { key, .. } => key,
        }
    }

    /// The row, when this entry is one.
    #[must_use]
    pub fn as_row(&self) -> Option<&ConfigRow> {
        match self {
            Entry::Row(row) => Some(row),
            Entry::Hidden { .. } => None,
        }
    }
}

use ConfigCategory as C;

/// Every configuration leaf ainb understands.
///
/// Ordered by section so the file reads like the TOML it describes. Adding a
/// field to the schema without adding an entry here fails
/// `every_leaf_has_an_entry`.
pub static CONFIG_REGISTRY: &[Entry] = &[
    // ── General ────────────────────────────────────────────────────────────
    Entry::Hidden {
        key: "version",
        why: "stamped from CARGO_PKG_VERSION on save; a migration marker, not a preference",
    },
    Entry::Row(ConfigRow {
        key: "default_container_template",
        category: C::General,
        label: "Default Container Template",
        help: "Container template used when a session does not name one",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "general.syntax_highlight",
        category: C::General,
        label: "Syntax Highlighting",
        help: "Colourise fenced code blocks in agent output (NO_COLOR still wins)",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "general.skill_install_real_homes",
        category: C::General,
        label: "Install Skills To Real Tool Homes",
        help: "Write installs to ~/.claude, ~/.codex … instead of ainb's managed sandbox",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "general.home",
        category: C::General,
        label: "State Directory",
        help: "Base dir for ainb state (<home>/.agents-in-a-box); blank = your home dir",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "presets.file",
        category: C::Presets,
        label: "Presets File",
        help: "Path to presets.toml, relative to the config dir or absolute",
        kind: RowKind::Text,
    }),
    // ── Authentication / agent defaults ────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "authentication.cli_provider",
        category: C::AgentDefaults,
        label: "Agent CLI",
        help: "Which agent CLI new sessions launch",
        kind: RowKind::Choice(&["claude", "codex", "gemini", "copilot"]),
    }),
    Entry::Row(ConfigRow {
        key: "authentication.claude_provider",
        category: C::Authentication,
        label: "Claude Authentication",
        help: "How Claude authenticates: subscription, API key, or a cloud gateway",
        kind: RowKind::Choice(&[
            "system_auth",
            "api_key",
            "amazon_bedrock",
            "google_vertex",
            "azure_foundry",
            "glm_zai",
            "llm_gateway",
        ]),
    }),
    Entry::Row(ConfigRow {
        key: "authentication.default_model",
        category: C::AgentDefaults,
        label: "Default Model",
        help: "Model alias passed to the agent CLI (e.g. sonnet, opus)",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "authentication.github_method",
        category: C::Authentication,
        label: "GitHub Auth Method",
        help: "How git operations authenticate to GitHub (unset = inherit the gh CLI)",
        kind: RowKind::Text,
    }),
    // ── Container templates ────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "container_templates.*.name",
        category: C::ContainerTemplates,
        label: "Template Name",
        help: "Identifier used to select this template",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.description",
        category: C::ContainerTemplates,
        label: "Template Description",
        help: "One line shown in the template picker",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.image_source.type",
        category: C::ContainerTemplates,
        label: "Image Source",
        help: "Prebuilt Image, a local Dockerfile, or the bundled ClaudeDocker build",
        kind: RowKind::Choice(&["Image", "Dockerfile", "ClaudeDocker"]),
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.image_source.name",
        category: C::ContainerTemplates,
        label: "Image Name",
        help: "Registry image reference, when the source is a prebuilt Image",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.image_source.path",
        category: C::ContainerTemplates,
        label: "Dockerfile Path",
        help: "Dockerfile to build, when the source is a Dockerfile",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.image_source.base_image",
        category: C::ContainerTemplates,
        label: "Base Image Override",
        help: "Base image for the ClaudeDocker build (unset = the bundled default)",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.image_source.build_args.*",
        category: C::ContainerTemplates,
        label: "Build Arg",
        help: "Docker build argument passed at image build time",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.working_dir",
        category: C::ContainerTemplates,
        label: "Working Directory",
        help: "Directory the repository mounts to inside the container",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.command",
        category: C::ContainerTemplates,
        label: "Command",
        help: "Container command argv (unset = the image default)",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.entrypoint",
        category: C::ContainerTemplates,
        label: "Entrypoint",
        help: "Container entrypoint argv (unset = the image default)",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.environment.*",
        category: C::ContainerTemplates,
        label: "Environment Variable",
        help: "Environment variable exported inside the container",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.user",
        category: C::ContainerTemplates,
        label: "Run As User",
        help: "User the container process runs as (unset = the image default)",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.memory_limit",
        category: C::ContainerTemplates,
        label: "Memory Limit (MB)",
        help: "Hard memory ceiling in megabytes",
        kind: RowKind::Number {
            min: 64,
            max: 1_048_576,
        },
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.cpu_limit",
        category: C::ContainerTemplates,
        label: "CPU Limit",
        help: "CPU shares; 0.5 is half a core",
        kind: RowKind::Float {
            min: 0.1,
            max: 1024.0,
        },
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.system_packages",
        category: C::ContainerTemplates,
        label: "System Packages",
        help: "apt packages installed into the image",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.npm_packages",
        category: C::ContainerTemplates,
        label: "NPM Packages",
        help: "npm packages installed globally in the image",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.python_packages",
        category: C::ContainerTemplates,
        label: "Python Packages",
        help: "pip packages installed into the image",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.ports",
        category: C::ContainerTemplates,
        label: "Exposed Ports",
        help: "Container ports published to the host",
        kind: RowKind::List(ListElement::Integer),
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.volumes",
        category: C::ContainerTemplates,
        label: "Volume Mounts",
        help: "Extra host→container mounts; edit with `ainb config edit`",
        kind: RowKind::Opaque,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.mount_ssh",
        category: C::ContainerTemplates,
        label: "Mount SSH Keys",
        help: "Bind ~/.ssh into the container so git over SSH works",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.config.mount_git_config",
        category: C::ContainerTemplates,
        label: "Mount Git Config",
        help: "Bind ~/.gitconfig so commits carry your identity",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.required_env",
        category: C::ContainerTemplates,
        label: "Required Env Vars",
        help: "Variables that must be present on the host before the template runs",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "container_templates.*.default_mcp_servers",
        category: C::ContainerTemplates,
        label: "Default MCP Servers",
        help: "MCP servers installed into containers built from this template",
        kind: RowKind::List(ListElement::Text),
    }),
    // ── MCP servers ────────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.name",
        category: C::McpServers,
        label: "Server Name",
        help: "Name the MCP server registers under in .mcp.json",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.description",
        category: C::McpServers,
        label: "Server Description",
        help: "One line shown in the MCP server list",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.installation.type",
        category: C::McpServers,
        label: "Install Method",
        help: "How the server binary is obtained before first launch",
        kind: RowKind::Choice(&["Npm", "Python", "Git", "PreInstalled", "Custom"]),
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.installation.package",
        category: C::McpServers,
        label: "Package",
        help: "npm or pip package name, for package installs",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.installation.version",
        category: C::McpServers,
        label: "Package Version",
        help: "Pinned package version (unset = latest)",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.installation.url",
        category: C::McpServers,
        label: "Repository URL",
        help: "Git remote to clone, for git installs",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.installation.branch",
        category: C::McpServers,
        label: "Repository Branch",
        help: "Branch to check out, for git installs (unset = the default branch)",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.installation.install_command",
        category: C::McpServers,
        label: "Install Command",
        help: "Command run inside the clone to build the server",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.installation.script",
        category: C::McpServers,
        label: "Install Script",
        help: "Shell script run to install a custom server",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.definition.type",
        category: C::McpServers,
        label: "Launch Kind",
        help: "Command: spawn an argv. Json: hand the agent a raw server block",
        kind: RowKind::Choice(&["Command", "Json"]),
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.definition.command",
        category: C::McpServers,
        label: "Launch Command",
        help: "Executable that starts the server",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.definition.args",
        category: C::McpServers,
        label: "Launch Arguments",
        help: "Arguments appended to the launch command",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.definition.env.*",
        category: C::McpServers,
        label: "Launch Environment",
        help: "Environment variable set for the server process",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.definition.config",
        category: C::McpServers,
        label: "Raw Server Block",
        help: "Verbatim JSON server definition; edit with `ainb config edit`",
        kind: RowKind::Opaque,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.required_env",
        category: C::McpServers,
        label: "Required Env Vars",
        help: "Host variables that must be set before this server can start",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.enabled_by_default",
        category: C::McpServers,
        label: "Enabled By Default",
        help: "Add this server to new sessions without being asked",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_servers.*.shared",
        category: C::McpPool,
        label: "Share Across Sessions",
        help: "Pool one process across sessions; turn off for stateful servers",
        kind: RowKind::Bool,
    }),
    // ── Workspace ──────────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "workspace_defaults.branch_prefix",
        category: C::Workspace,
        label: "Branch Prefix",
        help: "Prefix applied to branches created for new sessions",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "workspace_defaults.exclude_paths",
        category: C::Workspace,
        label: "Exclude Paths",
        help: "Substrings that exclude a directory from repository scanning",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "workspace_defaults.workspace_scan_paths",
        category: C::Workspace,
        label: "Scan Paths",
        help: "Extra directories scanned for git repositories, on top of the defaults",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "workspace_defaults.max_repositories",
        category: C::Workspace,
        label: "Max Repositories",
        help: "Cap on repositories listed in workspace search results",
        kind: RowKind::Number {
            min: 1,
            max: 100_000,
        },
    }),
    Entry::Row(ConfigRow {
        key: "workspace_defaults.worktree_collision_behavior",
        category: C::Workspace,
        label: "Worktree Collision",
        help: "When a worktree path already exists: rename automatically, or fail",
        kind: RowKind::Choice(&["auto_rename", "error"]),
    }),
    Entry::Row(ConfigRow {
        key: "workspace_defaults.scan_max_depth",
        category: C::Workspace,
        label: "Scan Depth",
        help: "Directory levels below each scan path the repo scanner descends",
        kind: RowKind::Number { min: 1, max: 16 },
    }),
    Entry::Row(ConfigRow {
        key: "workspace_defaults.scan_cache_ttl_secs",
        category: C::Workspace,
        label: "Scan Cache TTL",
        help: "Seconds a cached repository scan stays fresh before walking disk again",
        kind: RowKind::Number {
            min: 0,
            max: 604_800,
        },
    }), // ── UI preferences ─────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "ui_preferences.theme",
        category: C::Appearance,
        label: "Theme",
        help: "Colour theme for the TUI",
        kind: RowKind::Choice(&["dark", "light"]),
    }),
    Entry::Row(ConfigRow {
        key: "ui_preferences.show_container_status",
        category: C::Appearance,
        label: "Show Container Status",
        help: "Render container mode icons in the session list",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "ui_preferences.show_git_status",
        category: C::Appearance,
        label: "Show Git Status",
        help: "Render per-session git change counts in the session list",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "ui_preferences.show_session_menu_bar",
        category: C::Appearance,
        label: "Show Session Menu Bar",
        help: "Keep the Sessions keymap legend expanded (⇧M toggles it live)",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "ui_preferences.preferred_editor",
        category: C::Editor,
        label: "Preferred Editor",
        help: "Editor command used to open a session's worktree",
        kind: RowKind::Text,
    }),
    Entry::Hidden {
        key: "ui_preferences.session_filter",
        why: "the live Sessions status filter, cycled with a keypress and persisted so it survives a restart",
    },
    Entry::Hidden {
        key: "ui_preferences.home_sidebar_width",
        why: "written by the divider drag on the Home screen; a layout artefact, not a preference to type",
    },
    Entry::Hidden {
        key: "ui_preferences.sessions_sidebar_width",
        why: "written by the divider drag on the Sessions screen",
    },
    Entry::Hidden {
        key: "ui_preferences.sessions_sidebar_collapsed",
        why: "written when the Sessions sidebar is collapsed with a keypress",
    },
    Entry::Hidden {
        key: "ui_preferences.skill_manager_sources_width",
        why: "written by the divider drag on the Skill Manager screen",
    },
    Entry::Hidden {
        key: "ui_preferences.statusline_decision",
        why: "records whether the statusline prompt was answered; the wizard owns it, re-asking is the only way to change it",
    },
    Entry::Hidden {
        key: "ui_preferences.tmux_decision",
        why: "records whether the tmux.conf prompt was answered; owned by `ainb init` for the same reason",
    },
    Entry::Hidden {
        key: "ui_preferences.config_tree_expanded",
        why: "which nodes of this very screen's tree are open; written by the expand keypress, like the sidebar widths",
    },
    // ── UI tunables ────────────────────────────────────────────────────────
    // `[ui]`, not `[ui_preferences]`: that section is what the interface LOOKS
    // like and the TUI writes it back; these are cadences and query bounds a
    // user tunes for a slow terminal or a very large fleet.
    Entry::Row(ConfigRow {
        key: "ui.tick_rate_ms",
        category: C::Appearance,
        label: "Event Poll Interval",
        help: "Milliseconds between input polls; lower feels snappier and costs CPU",
        kind: RowKind::Number { min: 1, max: 1000 },
    }),
    Entry::Row(ConfigRow {
        key: "ui.app_tick_ms",
        category: C::Appearance,
        label: "App Tick Interval",
        help: "Milliseconds between heavy periodic passes (previews, animation, refreshes)",
        kind: RowKind::Number {
            min: 1,
            max: 60_000,
        },
    }),
    Entry::Row(ConfigRow {
        key: "ui.session_query_limit",
        category: C::Appearance,
        label: "Attention Query Limit",
        help: "Rows the attention-marker query reads per refresh",
        kind: RowKind::Number {
            min: 1,
            max: 100_000,
        },
    }),
    Entry::Row(ConfigRow {
        key: "ui.session_lookback_hours",
        category: C::Appearance,
        label: "Attention Lookback",
        help: "Hours back an event may still raise a session's attention marker",
        kind: RowKind::Number { min: 1, max: 8760 },
    }),
    Entry::Row(ConfigRow {
        key: "ui.inbox_list_limit",
        category: C::Appearance,
        label: "Inbox Row Limit",
        help: "Rows the Inbox lists; the query runs every render, so keep it modest",
        kind: RowKind::Number {
            min: 1,
            max: 100_000,
        },
    }),
    Entry::Row(ConfigRow {
        key: "ui.double_click_ms",
        category: C::Appearance,
        label: "Double-Click Window",
        help: "Milliseconds in which two clicks count as one double-click",
        kind: RowKind::Number { min: 50, max: 2000 },
    }),
    // ── Docker ─────────────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "docker.host",
        category: C::Docker,
        label: "Docker Host",
        help: "Docker endpoint, e.g. unix:///var/run/docker.sock (unset = autodetect)",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "docker.timeout",
        category: C::Docker,
        label: "Connection Timeout",
        help: "Seconds to wait on a Docker API call before giving up",
        kind: RowKind::Number { min: 1, max: 3600 },
    }),
    // ── Usage ──────────────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "usage.plan.id",
        category: C::Usage,
        label: "Subscription Plan",
        help: "Which subscription the burndown gauge measures against",
        kind: RowKind::Choice(&[
            "claude-pro",
            "claude-max",
            "claude-max5x",
            "cursor-pro",
            "custom",
            "none",
        ]),
    }),
    Entry::Row(ConfigRow {
        key: "usage.plan.monthly_usd",
        category: C::Usage,
        label: "Plan Monthly Cost",
        help: "Monthly plan price in USD; the denominator of the burndown gauge",
        kind: RowKind::Float {
            min: 0.0,
            max: 1_000_000.0,
        },
    }),
    Entry::Row(ConfigRow {
        key: "usage.plan.provider",
        category: C::Usage,
        label: "Plan Provider",
        help: "Which provider's spend counts against this plan",
        kind: RowKind::Choice(&["all", "claude", "codex", "cursor"]),
    }),
    Entry::Row(ConfigRow {
        key: "usage.plan.reset_day",
        category: C::Usage,
        label: "Billing Reset Day",
        help: "Day of month the plan's usage window restarts",
        kind: RowKind::Number { min: 1, max: 31 },
    }),
    Entry::Hidden {
        key: "usage.plan.set_at",
        why: "timestamp stamped when the plan was chosen; a provenance record, editing it changes nothing",
    },
    Entry::Row(ConfigRow {
        key: "usage.currency.code",
        category: C::Usage,
        label: "Currency Code",
        help: "ISO code costs are displayed in, e.g. GBP",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "usage.currency.symbol",
        category: C::Usage,
        label: "Currency Symbol",
        help: "Symbol prefixed to displayed costs",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "usage.currency.usd_rate",
        category: C::Usage,
        label: "USD Exchange Rate",
        help: "Multiplier applied to USD costs before display",
        kind: RowKind::Float {
            min: 0.000_001,
            max: 10_000.0,
        },
    }),
    Entry::Row(ConfigRow {
        key: "usage.model_aliases.*",
        category: C::Usage,
        label: "Model Alias",
        help: "Maps a raw model id in usage data onto a friendlier display name",
        kind: RowKind::Text,
    }),
    // ── Plugins ────────────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "plugins.enabled",
        category: C::Plugins,
        label: "Enabled Plugins",
        help: "Allowlist: when non-empty ONLY these plugins load",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Row(ConfigRow {
        key: "plugins.disabled",
        category: C::Plugins,
        label: "Disabled Plugins",
        help: "Denylist: these plugins are skipped (ignored when an allowlist is set)",
        kind: RowKind::List(ListElement::Text),
    }),
    Entry::Hidden {
        key: "plugins.*",
        why: "per-plugin values whose schema is the plugin's own [[config]] manifest; the screen renders those rows from the manifest, so duplicating them here would drift",
    },
    // ── Fleet ──────────────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "fleet.cost.session_usd",
        category: C::Fleet,
        label: "Session Budget (USD)",
        help: "Alert when any one session's lifetime spend crosses this",
        kind: RowKind::Float {
            min: 0.0,
            max: 1_000_000.0,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.cost.group_usd",
        category: C::Fleet,
        label: "Group Budget (USD)",
        help: "Alert when any one workspace group's lifetime spend crosses this",
        kind: RowKind::Float {
            min: 0.0,
            max: 1_000_000.0,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.cost.session_overrides.*",
        category: C::Fleet,
        label: "Session Budget Override",
        help: "Per-session USD ceiling, overriding the blanket session budget",
        kind: RowKind::Float {
            min: 0.0,
            max: 1_000_000.0,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.cost.group_overrides.*",
        category: C::Fleet,
        label: "Group Budget Override",
        help: "Per-group USD ceiling, overriding the blanket group budget",
        kind: RowKind::Float {
            min: 0.0,
            max: 1_000_000.0,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.interview.surface",
        category: C::Fleet,
        label: "Interview Surface",
        help: "native: Claude draws its own picker. fleet: hold the call for a remote answer",
        kind: RowKind::Choice(&["native", "fleet"]),
    }),
    Entry::Row(ConfigRow {
        key: "fleet.terminal",
        category: C::Fleet,
        label: "Jump-To Terminal",
        help: "Terminal the macOS Fleet app opens when you jump to a session",
        kind: RowKind::Choice(&["warp", "iterm", "ghostty", "terminal"]),
    }),
    Entry::Row(ConfigRow {
        key: "fleet.idle_min",
        category: C::Fleet,
        label: "Idle Threshold",
        help: "Minutes of quiet before a session reads IDLE, for tmux and hook sources alike",
        kind: RowKind::Number {
            min: 1,
            max: 10_080,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.transport",
        category: C::Fleet,
        label: "Send Transport",
        help: "How `ainb fleet send` delivers: tmux first, tmux only, or the broker",
        kind: RowKind::Choice(&["tmux", "tmux-only", "broker"]),
    }),
    Entry::Row(ConfigRow {
        key: "fleet.enrich",
        category: C::Fleet,
        label: "Row Enrichment",
        help: "Attach cost/hint enrichment to fleet rows; off spends no tokens",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.state_stale_ms",
        category: C::Fleet,
        label: "Sticky State Staleness",
        help: "Milliseconds before an ASK/ERR/WAIT/IDLE row falls back to a live scan; 0 = never",
        kind: RowKind::Number {
            min: 0,
            max: 86_400_000,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.healthy_state_stale_ms",
        category: C::Fleet,
        label: "Healthy State Staleness",
        help: "Milliseconds before a RUNNING/DONE row stops suppressing the live scan",
        kind: RowKind::Number {
            min: 0,
            max: 86_400_000,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.tmux_idle_after_secs",
        category: C::Fleet,
        label: "tmux Idle After",
        help: "Seconds of pane silence before tmux discovery calls a session between turns",
        kind: RowKind::Number {
            min: 1,
            max: 86_400,
        },
    }), // ── Fleet bridge ───────────────────────────────────────────────────────
    // Parsed by `fleet::bridge::config` off the same file; `ainb-core` carries
    // it as an opaque passthrough, so these rows are declared by hand and are
    // exempt from the schema walk (see EXTERNAL_PREFIXES).
    Entry::Row(ConfigRow {
        key: "fleet.bridge.outbound_enabled",
        category: C::Fleet,
        label: "Bridge Outbound Push",
        help: "Let the bridge message your phone when a session needs attention",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.outbound_poll_secs",
        category: C::Fleet,
        label: "Bridge Poll Interval",
        help: "Seconds between attention-inbox polls by the outbound worker",
        kind: RowKind::Number { min: 1, max: 3600 },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.response_timeout",
        category: C::Fleet,
        label: "Bridge Response Timeout",
        help: "Default seconds a channel waits for a session to answer",
        kind: RowKind::Number {
            min: 1,
            max: 86_400,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.telegram.token",
        category: C::Fleet,
        label: "Telegram Bot Token",
        help: "Literal, $ENV_VAR ref, or keychain:<service> ref",
        kind: RowKind::Secret,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.telegram.user_id",
        category: C::Fleet,
        label: "Telegram User ID",
        help: "The only Telegram account allowed to drive the bridge; a number or a $ENV ref",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.telegram.default_target",
        category: C::Fleet,
        label: "Telegram Default Target",
        help: "Session a bare message routes to when none is named",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.telegram.require_mention_in_groups",
        category: C::Fleet,
        label: "Telegram Require Mention",
        help: "In group chats, only act on messages that mention the bot",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.telegram.response_timeout",
        category: C::Fleet,
        label: "Telegram Response Timeout",
        help: "Seconds this channel waits for an answer, overriding the shared default",
        kind: RowKind::Number {
            min: 1,
            max: 86_400,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.slack.bot_token",
        category: C::Fleet,
        label: "Slack Bot Token",
        help: "xoxb- token. Literal, $ENV_VAR ref, or keychain:<service> ref",
        kind: RowKind::Secret,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.slack.app_token",
        category: C::Fleet,
        label: "Slack App Token",
        help: "xapp- socket-mode token. Literal, $ENV_VAR ref, or keychain:<service> ref",
        kind: RowKind::Secret,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.slack.user_id",
        category: C::Fleet,
        label: "Slack User ID",
        help: "The only Slack account allowed to drive the bridge",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.slack.default_target",
        category: C::Fleet,
        label: "Slack Default Target",
        help: "Session a bare message routes to when none is named",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.slack.listen_mode",
        category: C::Fleet,
        label: "Slack Listen Mode",
        help: "mentions: only @-mentions and DMs. all: every message in joined channels",
        kind: RowKind::Choice(&["mentions", "all"]),
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.slack.response_timeout",
        category: C::Fleet,
        label: "Slack Response Timeout",
        help: "Seconds this channel waits for an answer, overriding the shared default",
        kind: RowKind::Number {
            min: 1,
            max: 86_400,
        },
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.discord.token",
        category: C::Fleet,
        label: "Discord Bot Token",
        help: "Literal, $ENV_VAR ref, or keychain:<service> ref",
        kind: RowKind::Secret,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.discord.user_id",
        category: C::Fleet,
        label: "Discord User ID",
        help: "The only Discord account allowed to drive the bridge",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.discord.default_target",
        category: C::Fleet,
        label: "Discord Default Target",
        help: "Session a bare message routes to when none is named",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.discord.channel_id",
        category: C::Fleet,
        label: "Discord Channel ID",
        help: "Channel the bridge listens on and pushes into",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "fleet.bridge.discord.response_timeout",
        category: C::Fleet,
        label: "Discord Response Timeout",
        help: "Seconds this channel waits for an answer, overriding the shared default",
        kind: RowKind::Number {
            min: 1,
            max: 86_400,
        },
    }),
    // ── MCP pool ───────────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "mcp_pool.enabled",
        category: C::McpPool,
        label: "Shared MCP Pool",
        help: "Share one MCP server process across host sessions instead of one each",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "mcp_pool.idle_grace_secs",
        category: C::McpPool,
        label: "Idle Grace",
        help: "Seconds a pooled server survives after its last session detaches",
        kind: RowKind::Number {
            min: 0,
            max: 86_400,
        },
    }),
    Entry::Row(ConfigRow {
        key: "mcp_pool.monitor_refresh_secs",
        category: C::McpPool,
        label: "Monitor Refresh",
        help: "Auto-refresh cadence for the pool overlay while open; 0 = manual only",
        kind: RowKind::Number { min: 0, max: 3600 },
    }),
    Entry::Row(ConfigRow {
        key: "mcp_pool.daemon_idle_grace_secs",
        category: C::McpPool,
        label: "Daemon Idle Grace",
        help: "Seconds the pool daemon lingers with zero clients; 0 = never self-exit",
        kind: RowKind::Number {
            min: 0,
            max: 604_800,
        },
    }),
    // ── Usage client ───────────────────────────────────────────────────────
    // Core's half of usage. `[usage]` above is the burndown plugin's and is
    // read-only here; see `UsageClientConfig` for why these are not folded in.
    Entry::Row(ConfigRow {
        key: "usage_client.headroom_port",
        category: C::Usage,
        label: "Headroom Proxy Port",
        help: "Port the ainb-managed Headroom compression proxy listens on",
        kind: RowKind::Number {
            min: 1,
            max: 65_535,
        },
    }),
    Entry::Row(ConfigRow {
        key: "usage_client.fetch_timeout_secs",
        category: C::Usage,
        label: "Usage Fetch Timeout",
        help: "Seconds `ainb usage` waits for data; raise it against a huge session archive",
        kind: RowKind::Number {
            min: 1,
            max: 86_400,
        },
    }),
    Entry::Row(ConfigRow {
        key: "usage_client.codex_ttl_secs",
        category: C::Usage,
        label: "Codex Statusline TTL",
        help: "Seconds between Codex statusline usage refreshes",
        kind: RowKind::Number {
            min: 1,
            max: 86_400,
        },
    }),
    Entry::Row(ConfigRow {
        key: "usage_client.cache_db",
        category: C::Usage,
        label: "Usage Cache DB",
        help: "Path to the usage cache database; blank derives it under the state dir",
        kind: RowKind::Text,
    }),
    // ── Daemons ────────────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "daemons.stale_after_ms",
        category: C::Daemons,
        label: "Heartbeat Staleness",
        help: "Milliseconds without a heartbeat before a live-pid daemon reads stale",
        kind: RowKind::Number {
            min: 1_000,
            max: 86_400_000,
        },
    }),
    Entry::Row(ConfigRow {
        key: "daemons.attention_stale_after_ms",
        category: C::Daemons,
        label: "Bridge Push Staleness",
        help: "Milliseconds without a successful attention poll before the bridge reads broken",
        kind: RowKind::Number {
            min: 1_000,
            max: 86_400_000,
        },
    }),
    Entry::Row(ConfigRow {
        key: "notifyd.os_debounce_secs",
        category: C::Daemons,
        label: "OS Notification Debounce",
        help: "Seconds one session/event pair waits before it may notify again",
        kind: RowKind::Number {
            min: 0,
            max: 86_400,
        },
    }),
    Entry::Row(ConfigRow {
        key: "notifyd.approval_timeout_secs",
        category: C::Daemons,
        label: "Approval Timeout",
        help: "Seconds an unanswered permission request waits before it is auto-DENIED",
        kind: RowKind::Number { min: 1, max: 630 },
    }),
    // ── Web dashboard ──────────────────────────────────────────────────────
    // No token row: `--token` puts it in the OS keychain, never in this file.
    Entry::Row(ConfigRow {
        key: "web.listen",
        category: C::Web,
        label: "Listen Address",
        help: "host:port `ainb web` binds by default; --listen still overrides",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "web.read_only",
        category: C::Web,
        label: "Read-Only",
        help: "Serve viewer-only: the live terminal and POST /api/answer are refused",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "web.insecure_bind",
        category: C::Web,
        label: "Allow Insecure Bind",
        help: "Permit a non-loopback bind with no token; honoured only with read-only",
        kind: RowKind::Bool,
    }),
    // ── ACP adapters ───────────────────────────────────────────────────────
    Entry::Row(ConfigRow {
        key: "acp.adapters.*.command",
        category: C::Acp,
        label: "Adapter Command",
        help: "Executable to spawn for this adapter; blank resolves its name on PATH",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "acp.adapters.*.permission_mode",
        category: C::Acp,
        label: "Permission Mode",
        help: "Mode pinned at session/new and re-asserted after every session/load",
        kind: RowKind::Choice(&["default", "acceptEdits", "bypassPermissions", "plan"]),
    }),
    // ── Sections owned by other crates ─────────────────────────────────────
    // Same file, different parser. See EXTERNAL_PREFIXES.
    Entry::Row(ConfigRow {
        key: "skills.catalog_release",
        category: C::Skills,
        label: "Catalog Release",
        help: "Release tag of the curated skills catalog to fetch (default: latest)",
        kind: RowKind::Text,
    }),
    Entry::Row(ConfigRow {
        key: "skills.api_key",
        category: C::Skills,
        label: "Skills API Key",
        help: "skills.sh API key. Literal, $ENV_VAR ref, or keychain:<service> ref",
        kind: RowKind::Secret,
    }),
    Entry::Row(ConfigRow {
        key: "session_reader.incremental_window_days",
        category: C::SessionReader,
        label: "Incremental Window",
        help: "Days of session history re-aggregated on each scan; older files use the cached aggregate",
        kind: RowKind::Number {
            min: 1,
            max: 36_500,
        },
    }),
    // ── Hangar daemon ──────────────────────────────────────────────────────
    // A SECOND write backend: these live in the `daemon_config` SQLite table,
    // not in config.toml, and are written over the daemon's control socket.
    // Mirrored from `ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY`
    // (which cannot be iterated into a `const`), and pinned to it by
    // `hangar_daemon_rows_match_the_daemon_registry`.
    Entry::Row(ConfigRow {
        key: "hangar_daemon.autostandup.enabled",
        category: C::HangarDaemon,
        label: "Auto-standup",
        help: "Globally enable the auto-standup watcher (opt-in)",
        kind: RowKind::Bool,
    }),
    Entry::Row(ConfigRow {
        key: "hangar_daemon.autostandup.stagnant_min",
        category: C::HangarDaemon,
        label: "Stagnant Minutes",
        help: "Minutes a session must be idle before a standup fires",
        kind: RowKind::Number { min: 1, max: 1440 },
    }),
    Entry::Row(ConfigRow {
        key: "hangar_daemon.autostandup.cooldown_min",
        category: C::HangarDaemon,
        label: "Cooldown Minutes",
        help: "Per-session minutes between successive standups",
        kind: RowKind::Number { min: 1, max: 1440 },
    }),
    Entry::Row(ConfigRow {
        key: "hangar_daemon.autostandup.max_concurrent",
        category: C::HangarDaemon,
        label: "Max Concurrent",
        help: "Maximum simultaneous in-flight standups",
        kind: RowKind::Number { min: 1, max: 64 },
    }),
    Entry::Row(ConfigRow {
        key: "hangar_daemon.card_agent.default",
        category: C::HangarDaemon,
        label: "Default Agent",
        help: "Host-wide default provider backend for new cards",
        kind: RowKind::Choice(&["claude", "codex", "copilot"]),
    }),
    Entry::Row(ConfigRow {
        key: "hangar_daemon.workspace.creation_disabled",
        category: C::HangarDaemon,
        label: "Lock Workspace Creation",
        help: "Refuse every new-workspace create on this instance (self-host lockdown)",
        kind: RowKind::Bool,
    }),
];

/// The registry-key prefix for a knob backed by the Hangar daemon's
/// `daemon_config` SQLite table rather than config.toml.
///
/// Strip it to get the `daemon_config` key
/// (`hangar_daemon.autostandup.enabled` → `autostandup.enabled`).
pub const HANGAR_DAEMON_PREFIX: &str = "hangar_daemon.";

/// The `daemon_config` key a `hangar_daemon.*` registry key names, or `None`
/// for any other key.
#[must_use]
pub fn hangar_daemon_key(key: &str) -> Option<&str> {
    key.strip_prefix(HANGAR_DAEMON_PREFIX)
}

/// Top-level prefixes that live in `config.toml` but not in
/// [`AppConfig`](crate::config::AppConfig)'s serde shape.
///
/// `[fleet.bridge]` is carried as an opaque `toml::Value` passthrough (its
/// tokens are parsed by `fleet::bridge::config`), and `[skills]` /
/// `[session_reader]` are parsed off the same file by `ainb-cli` and the
/// session-reader plugin. `hangar_daemon.` is not in this file at all: it is
/// the Hangar daemon's `daemon_config` SQLite table, reached over the control
/// socket. Their leaves are registered by hand above and are skipped by the
/// schema walk in both directions: nothing here can prove they exist, so
/// nothing here should claim they don't.
pub const EXTERNAL_PREFIXES: &[&str] = &[
    "fleet.bridge.",
    "skills.",
    "session_reader.",
    "hangar_daemon.",
];

/// Registry keys whose schema field is an `Option`: clearing the widget must
/// REMOVE the key rather than store an empty value.
///
/// Storing `""` in an `Option<String>` yields `Some("")`, not `None` —
/// `docker.host` then becomes an empty endpoint that Docker tries to dial, and
/// `preferred_editor` an empty command. Every other row keeps its empty value,
/// because those fields have a serde default and removing the key would
/// silently restore that default instead of honouring the edit.
///
/// Hand-written, like [`CONFIG_REGISTRY`] itself, and proved against the schema
/// by `optional_keys_match_the_schema` — which DERIVES the true set by dropping
/// each leaf and checking whether serde puts it back.
pub const OPTIONAL_KEYS: &[&str] = &[
    "acp.adapters.*.command",
    "authentication.github_method",
    "container_templates.*.config.command",
    "container_templates.*.config.cpu_limit",
    "container_templates.*.config.entrypoint",
    "container_templates.*.config.environment.*",
    "container_templates.*.config.image_source.base_image",
    "container_templates.*.config.image_source.build_args.*",
    "container_templates.*.config.memory_limit",
    "container_templates.*.config.user",
    "docker.host",
    "fleet.cost.group_overrides.*",
    "fleet.cost.group_usd",
    "fleet.cost.session_overrides.*",
    "fleet.cost.session_usd",
    "fleet.interview.surface",
    "fleet.terminal",
    "general.home",
    "ui_preferences.preferred_editor",
    "usage.model_aliases.*",
    "usage_client.cache_db",
];

/// True when clearing this row's widget should remove the key.
#[must_use]
pub fn is_optional(key: &str) -> bool {
    OPTIONAL_KEYS.contains(&registry_key(key).as_str())
}

/// Registry paths whose direct children are user-chosen map keys rather than
/// schema field names. The child segment normalises to `*`.
const MAP_PARENTS: &[&str] = &[
    "acp.adapters",
    "container_templates",
    "container_templates.*.config.environment",
    "container_templates.*.config.image_source.build_args",
    "mcp_servers",
    "mcp_servers.*.definition.env",
    "usage.model_aliases",
    "fleet.cost.session_overrides",
    "fleet.cost.group_overrides",
    "plugins",
];

/// Registry paths that are leaves even though they serialize as a table: the
/// structure below them is not ours to describe.
const OPAQUE_ROOTS: &[&str] = &["plugins.*", "mcp_servers.*.definition.config"];

/// `[plugins]` mixes real fields with the flattened per-plugin value map, so
/// the map-key rule has to skip these two names.
const PLUGIN_FIELDS: &[&str] = &["enabled", "disabled"];

fn is_map_parent(path: &str) -> bool {
    MAP_PARENTS.contains(&path)
}

fn is_opaque_root(path: &str) -> bool {
    OPAQUE_ROOTS.contains(&path)
}

/// True when `key` belongs to a section this registry describes but the schema
/// walk cannot see. See [`EXTERNAL_PREFIXES`].
#[must_use]
pub fn is_external(key: &str) -> bool {
    EXTERNAL_PREFIXES.iter().any(|prefix| key.starts_with(prefix))
}

/// Normalise a concrete dotted path into the key the registry uses: map keys
/// collapse to `*`, and descent stops at an opaque root.
///
/// `mcp_servers.context7.shared` → `mcp_servers.*.shared`;
/// `plugins.learnings.learnings_dir` → `plugins.*`.
#[must_use]
pub fn registry_key(concrete: &str) -> String {
    let mut path = String::new();
    for segment in parse_dot_key(concrete) {
        if is_opaque_root(&path) {
            return path;
        }
        path = child_key(&path, &segment);
    }
    path
}

/// Extend a registry path by one segment, collapsing a map key to `*`.
/// Shared by [`registry_key`] and the schema walk in the tests, so a concrete
/// path and a walked path can never normalise differently.
fn child_key(path: &str, segment: &str) -> String {
    let normalised =
        if is_map_parent(path) && !(path == "plugins" && PLUGIN_FIELDS.contains(&segment)) {
            "*"
        } else {
            segment
        };
    if path.is_empty() {
        normalised.to_string()
    } else {
        format!("{path}.{normalised}")
    }
}

/// Look up the entry for a concrete or already-normalised dotted path.
#[must_use]
pub fn entry(key: &str) -> Option<&'static Entry> {
    let normalised = registry_key(key);
    CONFIG_REGISTRY.iter().find(|entry| entry.key() == normalised)
}

/// Look up the *row* for a dotted path. Hidden leaves return `None`.
#[must_use]
pub fn row(key: &str) -> Option<&'static ConfigRow> {
    entry(key).and_then(Entry::as_row)
}

/// Every user-facing row, in registry order.
pub fn rows() -> impl Iterator<Item = &'static ConfigRow> {
    CONFIG_REGISTRY.iter().filter_map(Entry::as_row)
}

// --- Dotted-path navigation -------------------------------------------------
//
// Lifted out of `cli/config_cmd.rs` so the CLI, the settings screen and the
// registry's own validation all read and write a leaf the same way.

/// Split a dot-notation key into its segments, dropping empties.
#[must_use]
pub fn parse_dot_key(key: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = key.chars();
    while let Some(c) = chars.next() {
        match c {
            // A quoted segment holds a key that is not bare — a model id like
            // `gpt-4.1`, an MCP server called `my.server`. Splitting those on
            // `.` invents nested tables and corrupts the file, so honour the
            // quotes the same way TOML itself does.
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '.' if !quoted => {
                let part = current.trim().to_string();
                if !part.is_empty() {
                    parts.push(part);
                }
                current.clear();
            }
            other => current.push(other),
        }
    }
    let last = current.trim().to_string();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

/// Render one path segment for use in a dotted key, quoting it when it is not
/// a bare TOML key.
///
/// The inverse of [`parse_dot_key`]'s quote handling: a segment containing a
/// `.` (or a quote, or whitespace) has to go back as `"gpt-4.1"`, or the next
/// parse splits it apart again.
#[must_use]
pub fn quote_key_segment(segment: &str) -> String {
    let bare = !segment.is_empty()
        && segment.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        segment.to_string()
    } else {
        format!("\"{}\"", segment.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Navigate a TOML value tree using dot-notation keys.
pub fn navigate_toml<'a>(value: &'a toml::Value, key: &str) -> Result<&'a toml::Value> {
    let parts = parse_dot_key(key);
    if parts.is_empty() {
        return Err(anyhow!("Empty key"));
    }

    let mut current = value;
    for (i, part) in parts.iter().enumerate() {
        if let toml::Value::Table(table) = current {
            current = table.get(part.as_str()).ok_or_else(|| {
                let path = parts[..=i].join(".");
                anyhow!("Key '{path}' not found in configuration")
            })?;
        } else {
            let path = parts[..i].join(".");
            return Err(anyhow!("Cannot index into non-table value at '{path}'"));
        }
    }

    Ok(current)
}

/// Set a value in a TOML tree using dot-notation keys, creating intermediate
/// tables. Performs no validation; prefer [`set_validated`].
pub fn set_toml_value(root: &mut toml::Value, key: &str, raw_value: &str) -> Result<()> {
    insert_at(root, key, parse_toml_scalar(raw_value))
}

/// Insert an already-built value at a dotted path, creating intermediate tables.
///
/// Public so a writer that has already produced a typed value (the tree-expansion
/// array, say) does not have to render it back to a string just to have
/// [`set_validated`] parse it again.
pub fn insert_at(root: &mut toml::Value, key: &str, value: toml::Value) -> Result<()> {
    let parts = parse_dot_key(key);
    if parts.is_empty() {
        return Err(anyhow!("Empty key"));
    }

    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        current = current
            .as_table_mut()
            .ok_or_else(|| anyhow!("Cannot set key in non-table value"))?
            .entry(part)
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    }

    let table = current
        .as_table_mut()
        .ok_or_else(|| anyhow!("Cannot set key in non-table value"))?;

    table.insert(parts[parts.len() - 1].clone(), value);
    Ok(())
}

/// Parse a raw string into the most appropriate TOML scalar.
#[must_use]
pub fn parse_toml_scalar(raw: &str) -> toml::Value {
    if raw.eq_ignore_ascii_case("true") {
        return toml::Value::Boolean(true);
    }
    if raw.eq_ignore_ascii_case("false") {
        return toml::Value::Boolean(false);
    }
    if let Ok(i) = raw.parse::<i64>() {
        return toml::Value::Integer(i);
    }
    if raw.contains('.') {
        if let Ok(f) = raw.parse::<f64>() {
            return toml::Value::Float(f);
        }
    }
    toml::Value::String(raw.to_string())
}

// --- Validation -------------------------------------------------------------

/// Validate a raw string against the registry entry for `key`, returning the
/// TOML value to store.
///
/// `ainb config set` used to write any dotted path it was handed, so a typo
/// landed silently in config.toml and was dropped on the next load, and an
/// out-of-range number was only noticed when something downstream misbehaved.
/// Both now fail here, before anything is written.
///
/// Hidden leaves are accepted with plain scalar parsing: they are real schema
/// keys owned by a wizard or the layout code, so refusing them outright would
/// remove an escape hatch, but there is no user-facing type to check them
/// against.
pub fn validate(key: &str, raw: &str) -> Result<toml::Value> {
    let normalised = registry_key(key);
    let Some(found) = entry(&normalised) else {
        let hint = suggestions(&normalised);
        bail!("Unknown config key '{key}'.{hint}");
    };

    let Some(row) = found.as_row() else {
        // Hidden leaf: internal state, no declared widget to validate against.
        return Ok(parse_toml_scalar(raw));
    };

    // A key BELOW an opaque root is a user-extensible namespace, not a typo:
    // `plugins.<name>.<field>` normalises to `plugins.*`, whose shape is the
    // plugin's own `[[config]]` manifest and not ours to declare. Refusing it
    // broke `ainb config set plugins.learnings.learnings_dir`, which worked
    // before the registry existed. Setting the opaque root ITSELF is still
    // refused below — that is the structured value `ainb config edit` is for.
    // `registry_key` stops descending at an opaque root, so a concrete key with
    // MORE segments than its normalised form is one that reaches inside.
    if matches!(row.kind, RowKind::Opaque)
        && parse_dot_key(key).len() > parse_dot_key(&normalised).len()
    {
        return Ok(parse_toml_scalar(raw));
    }

    match row.kind {
        RowKind::Text | RowKind::Secret => Ok(toml::Value::String(raw.to_string())),
        RowKind::Bool => match raw.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(toml::Value::Boolean(true)),
            "false" => Ok(toml::Value::Boolean(false)),
            other => bail!("'{key}' is a true/false setting, got '{other}'"),
        },
        RowKind::Number { min, max } => {
            let n: i64 = raw
                .trim()
                .parse()
                .map_err(|_| anyhow!("'{key}' takes a whole number, got '{raw}'"))?;
            if n < min || n > max {
                bail!("'{key}' must be between {min} and {max}, got {n}");
            }
            Ok(toml::Value::Integer(n))
        }
        RowKind::Float { min, max } => {
            let f: f64 =
                raw.trim().parse().map_err(|_| anyhow!("'{key}' takes a number, got '{raw}'"))?;
            if !f.is_finite() || f < min || f > max {
                bail!("'{key}' must be between {min} and {max}, got {raw}");
            }
            Ok(toml::Value::Float(f))
        }
        RowKind::Choice(options) => {
            let value = raw.trim();
            if options.contains(&value) {
                Ok(toml::Value::String(value.to_string()))
            } else {
                bail!(
                    "'{key}' must be one of: {}. Got '{value}'",
                    options.join(", ")
                );
            }
        }
        RowKind::List(element) => {
            let mut items = Vec::new();
            for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                items.push(match element {
                    ListElement::Text => toml::Value::String(item.to_string()),
                    ListElement::Integer => toml::Value::Integer(
                        item.parse()
                            .map_err(|_| anyhow!("'{key}' takes whole numbers, got '{item}'"))?,
                    ),
                });
            }
            Ok(toml::Value::Array(items))
        }
        RowKind::Opaque => bail!(
            "'{key}' holds a structured value that cannot be set from the command line; use `ainb config edit`"
        ),
    }
}

/// Validate `raw` against the registry, then write it into `root` at `key`.
///
/// An emptied widget on an [`OPTIONAL_KEYS`] row REMOVES the key instead of
/// storing an empty value: `Option<String>` deserializes `""` as `Some("")`,
/// which is how clearing `docker.host` used to leave Docker dialling an empty
/// endpoint. Every other row keeps its empty value, because removing a key with
/// a serde default would silently restore that default rather than honour the
/// edit.
pub fn set_validated(root: &mut toml::Value, key: &str, raw: &str) -> Result<()> {
    if raw.trim().is_empty() && is_optional(key) {
        remove_at(root, key);
        return Ok(());
    }
    let value = validate(key, raw)?;
    insert_at(root, key, value)
}

/// Delete the leaf at a dotted path, leaving its parent tables in place.
/// Returns whether anything was removed.
pub fn remove_at(root: &mut toml::Value, key: &str) -> bool {
    let parts = parse_dot_key(key);
    let Some((last, parents)) = parts.split_last() else {
        return false;
    };
    let mut current = root;
    for part in parents {
        match current.as_table_mut().and_then(|table| table.get_mut(part.as_str())) {
            Some(child) => current = child,
            None => return false,
        }
    }
    current
        .as_table_mut()
        .is_some_and(|table| table.remove(last.as_str()).is_some())
}

/// " Did you mean …" tail for an unknown key, or a pointer at `ainb config
/// show` when nothing looks close. Cheap linear scan over ~120 entries.
fn suggestions(key: &str) -> String {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    let head = key.split('.').next().unwrap_or(key);
    let mut hits: Vec<&str> = CONFIG_REGISTRY
        .iter()
        .map(Entry::key)
        .filter(|candidate| {
            candidate.rsplit('.').next() == Some(leaf) || candidate.starts_with(head)
        })
        .take(4)
        .collect();
    hits.dedup();
    if hits.is_empty() {
        " Run `ainb config show` to see the keys that exist.".to_string()
    } else {
        format!(" Did you mean: {}?", hits.join(", "))
    }
}

// --- Settings-screen conversion ---------------------------------------------

impl ConfigRow {
    /// Build the settings row the screen renders, seeded from `current`: the
    /// value at this row's path in the loaded config, which already carries the
    /// serde defaults. `None` (an absent optional key) seeds an empty widget.
    ///
    /// Mirrors `config_value_for_field`, which does the same job for a plugin's
    /// `[[config]]` fields.
    #[must_use]
    pub fn to_setting(&self, current: Option<&toml::Value>) -> ConfigSetting {
        ConfigSetting {
            key: self.key.to_string(),
            label: self.label.to_string(),
            value: self.to_value(current),
            description: self.help.to_string(),
        }
    }

    /// The widget value for this row, seeded from `current`.
    ///
    /// `Float` renders as text: [`ConfigValue`] has no float variant, and
    /// rounding a currency rate or a CPU share into an integer would silently
    /// corrupt it.
    ///
    /// [`RowKind::Secret`] resolves a `$VAR` reference against the process
    /// environment, which is free, and deliberately does NOT touch the
    /// keychain. `build_rows` runs before the TUI's first paint, and a
    /// `keychain:` reference costs an in-process keyring read plus a
    /// `/usr/bin/security` shell-out, each bounded at 5s — with the bridge's
    /// four token rows and the skills key all set, that was up to ~50s of
    /// blocking on startup against a locked keychain. A `keychain:` row is
    /// reported as configured on the strength of having a reference at all;
    /// whether the secret is actually retrievable is a question for the moment
    /// something needs its value.
    #[must_use]
    pub fn to_value(&self, current: Option<&toml::Value>) -> ConfigValue {
        match self.kind {
            RowKind::Secret => {
                let reference = current.map(scalar_text).unwrap_or_default();
                let trimmed = reference.trim();
                let resolved = if trimmed.is_empty() {
                    false
                } else if trimmed.starts_with("keychain:") {
                    true
                } else {
                    !crate::fleet::bridge::secrets::resolve_secret(&reference).trim().is_empty()
                };
                ConfigValue::Secret(crate::app::state::SecretValue {
                    reference,
                    resolved,
                })
            }
            RowKind::Bool => {
                ConfigValue::Bool(current.and_then(toml::Value::as_bool).unwrap_or(false))
            }
            RowKind::Number { .. } => match current.and_then(toml::Value::as_integer) {
                Some(n) => ConfigValue::Number(n),
                // An unset OPTIONAL row is blank, not `0`. Rendering `0` was a
                // false claim about the config — `memory_limit` has a `min` of
                // 64, so confirming the row unchanged sent `"0"` and the edit
                // was rejected. Text also gives the row a way to be cleared,
                // which a number widget has no way to express.
                None if is_optional(self.key) => ConfigValue::Text(String::new()),
                None => ConfigValue::Number(0),
            },
            RowKind::Choice(options) => {
                let selected = current.map(scalar_text).unwrap_or_default();
                let idx = options.iter().position(|o| *o == selected).unwrap_or(0);
                ConfigValue::Choice(options.iter().map(|o| (*o).to_string()).collect(), idx)
            }
            RowKind::Text | RowKind::Float { .. } | RowKind::List(_) | RowKind::Opaque => {
                ConfigValue::Text(current.map(scalar_text).unwrap_or_default())
            }
        }
    }
}

/// Render a TOML value as the plain string a text widget edits. Arrays join
/// with ", " to match the comma-separated form [`validate`] parses back.
#[must_use]
pub fn scalar_text(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(dt) => dt.to_string(),
        toml::Value::Array(items) => items.iter().map(scalar_text).collect::<Vec<_>>().join(", "),
        toml::Value::Table(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::SessionFilter;
    use crate::config::container::{ImageSource, VolumeMount};
    use crate::config::{
        AcpAdapterConfig, AcpConfig, AppConfig, AuthenticationConfig, ClaudeAuthProvider,
        CliProvider, ContainerTemplate, ContainerTemplateConfig, CostBudgetConfig, CurrencyConfig,
        DaemonsConfig, DockerConfig, FleetConfig, GeneralConfig, InterviewConfig, McpInstallation,
        McpPoolConfig, McpServerConfig, McpServerDefinition, NotifydConfig, PluginsConfig,
        PresetsConfig, StatuslineDecision, TmuxDecision, UiConfig, UiPreferences,
        UsageClientConfig, UsageConfig, UsagePlan, UsagePlanId, UsagePlanProvider, WebServerConfig,
        WorkspaceDefaults, WorktreeCollisionBehavior,
    };
    use std::collections::{BTreeMap, HashMap};

    /// A config with **every** leaf present.
    ///
    /// Deliberately written as exhaustive struct literals with no
    /// `..Default::default()`: adding a field to any config struct fails to
    /// compile here, which is the first half of the drift guard. The second
    /// half is [`every_leaf_has_an_entry`], which needs every optional field
    /// populated, because an absent `Option` serializes to nothing and would hide a
    /// missing registry row.
    ///
    /// Enum-valued fields carry real variants rather than hand-written strings
    /// so [`choice_options_match_serde`] checks the registry's choice lists
    /// against serde's actual output.
    fn fully_populated() -> AppConfig {
        AppConfig {
            version: "9.9.9".to_string(),
            authentication: AuthenticationConfig {
                cli_provider: CliProvider::Codex,
                claude_provider: ClaudeAuthProvider::ApiKey,
                default_model: "opus".to_string(),
                github_method: Some("gh".to_string()),
            },
            default_container_template: "claude-dev".to_string(),
            container_templates: container_templates(),
            mcp_servers: mcp_servers(),
            workspace_defaults: WorkspaceDefaults {
                branch_prefix: "agents/".to_string(),
                exclude_paths: vec!["node_modules".to_string()],
                workspace_scan_paths: vec!["/tmp/repos".into()],
                max_repositories: 500,
                worktree_collision_behavior: WorktreeCollisionBehavior::Error,
                scan_max_depth: 4,
                scan_cache_ttl_secs: 1800,
            },
            ui_preferences: UiPreferences {
                theme: "light".to_string(),
                show_container_status: true,
                show_git_status: false,
                show_session_menu_bar: true,
                session_filter: SessionFilter::ActiveOnly,
                preferred_editor: Some("nvim".to_string()),
                home_sidebar_width: Some(30),
                sessions_sidebar_width: Some(28),
                sessions_sidebar_collapsed: Some(true),
                skill_manager_sources_width: Some(32),
                statusline_decision: StatuslineDecision::Installed,
                tmux_decision: TmuxDecision::Declined,
                config_tree_expanded: vec!["Fleet|fleet".to_string()],
            },
            docker: DockerConfig {
                host: Some("unix:///var/run/docker.sock".to_string()),
                timeout: 60,
            },
            usage: UsageConfig {
                plan: Some(UsagePlan {
                    id: UsagePlanId::ClaudeMax5x,
                    monthly_usd: 100.0,
                    provider: UsagePlanProvider::Claude,
                    reset_day: 14,
                    set_at: "2026-08-28T00:00:00Z".to_string(),
                }),
                currency: CurrencyConfig {
                    code: "GBP".to_string(),
                    symbol: "£".to_string(),
                    usd_rate: 0.78,
                },
                model_aliases: HashMap::from([("claude-opus-5".to_string(), "opus".to_string())]),
            },
            plugins: PluginsConfig {
                enabled: vec!["learnings".to_string()],
                disabled: vec!["burndown".to_string()],
                values: BTreeMap::from([(
                    "learnings".to_string(),
                    toml::Value::Table(toml::map::Map::from_iter([(
                        "learnings_dir".to_string(),
                        toml::Value::String("~/kb".to_string()),
                    )])),
                )]),
            },
            presets: PresetsConfig {
                file: "../presets.toml".to_string(),
            },
            fleet: FleetConfig {
                cost: CostBudgetConfig {
                    session_usd: Some(5.0),
                    group_usd: Some(25.0),
                    session_overrides: HashMap::from([("abc123".to_string(), 50.0)]),
                    group_overrides: HashMap::from([("infra".to_string(), 100.0)]),
                },
                interview: InterviewConfig {
                    surface: Some("fleet".to_string()),
                },
                terminal: Some("ghostty".to_string()),
                idle_min: 7,
                transport: "broker".to_string(),
                enrich: false,
                state_stale_ms: 120_000,
                healthy_state_stale_ms: 240_000,
                tmux_idle_after_secs: 90,
                // `[fleet.bridge]` is an opaque passthrough parsed elsewhere;
                // its rows are hand-declared and walk-exempt. See
                // EXTERNAL_PREFIXES.
                bridge: None,
            },
            mcp_pool: McpPoolConfig {
                enabled: true,
                idle_grace_secs: 300,
                monitor_refresh_secs: 2,
                daemon_idle_grace_secs: 900,
            },
            general: GeneralConfig {
                syntax_highlight: false,
                skill_install_real_homes: false,
                home: Some("/tmp/ainb-home".to_string()),
            },
            ui: UiConfig {
                tick_rate_ms: 16,
                app_tick_ms: 500,
                session_query_limit: 250,
                session_lookback_hours: 12,
                inbox_list_limit: 100,
                double_click_ms: 400,
            },
            daemons: DaemonsConfig {
                stale_after_ms: 120_000,
                attention_stale_after_ms: 60_000,
            },
            usage_client: UsageClientConfig {
                headroom_port: 9787,
                fetch_timeout_secs: 300,
                codex_ttl_secs: 30,
                cache_db: Some("/tmp/usage.db".to_string()),
            },
            notifyd: NotifydConfig {
                os_debounce_secs: 30,
                approval_timeout_secs: 300,
            },
            web: WebServerConfig {
                listen: "0.0.0.0:8420".to_string(),
                read_only: true,
                insecure_bind: true,
            },
            acp: AcpConfig {
                adapters: HashMap::from([(
                    "claude-agent-acp".to_string(),
                    AcpAdapterConfig {
                        command: Some("/opt/bin/claude-agent-acp".to_string()),
                        permission_mode: "acceptEdits".to_string(),
                    },
                )]),
            },
        }
    }

    /// One template per [`ImageSource`] variant, because a single template can only be
    /// one of them, so the variant-specific leaves need three entries. Map keys
    /// normalise to `*`, so they all fold into the same registry paths.
    fn container_templates() -> HashMap<String, ContainerTemplate> {
        let base = |image_source: ImageSource| ContainerTemplateConfig {
            image_source,
            working_dir: "/workspace".to_string(),
            command: Some(vec!["bash".to_string()]),
            entrypoint: Some(vec!["/app/start.sh".to_string()]),
            environment: HashMap::from([("NODE_ENV".to_string(), "development".to_string())]),
            user: Some("claude-user".to_string()),
            memory_limit: Some(4096),
            cpu_limit: Some(2.0),
            system_packages: vec!["git".to_string()],
            npm_packages: vec!["@anthropic-ai/claude-code".to_string()],
            python_packages: vec!["uv".to_string()],
            ports: vec![3000, 5173],
            volumes: vec![VolumeMount {
                host_path: "/tmp".to_string(),
                container_path: "/tmp".to_string(),
                read_only: true,
            }],
            mount_ssh: true,
            mount_git_config: true,
        };
        let template = |name: &str, image_source: ImageSource| ContainerTemplate {
            name: name.to_string(),
            description: "test template".to_string(),
            config: base(image_source),
            required_env: vec!["ANTHROPIC_API_KEY".to_string()],
            default_mcp_servers: vec!["context7".to_string()],
        };
        HashMap::from([
            (
                "image".to_string(),
                template(
                    "image",
                    ImageSource::Image {
                        name: "node:20-slim".to_string(),
                    },
                ),
            ),
            (
                "dockerfile".to_string(),
                template(
                    "dockerfile",
                    ImageSource::Dockerfile {
                        path: "/tmp/Dockerfile".into(),
                        build_args: HashMap::from([(
                            "RUST_VERSION".to_string(),
                            "1.90".to_string(),
                        )]),
                    },
                ),
            ),
            (
                "claude-docker".to_string(),
                template(
                    "claude-docker",
                    ImageSource::ClaudeDocker {
                        base_image: Some("debian:bookworm".to_string()),
                        build_args: HashMap::from([("TZ".to_string(), "UTC".to_string())]),
                    },
                ),
            ),
        ])
    }

    /// One server per [`McpInstallation`] / [`McpServerDefinition`] variant, for
    /// the same reason as [`container_templates`].
    fn mcp_servers() -> HashMap<String, McpServerConfig> {
        let server = |name: &str,
                      installation: McpInstallation,
                      definition: McpServerDefinition| McpServerConfig {
            name: name.to_string(),
            description: "test server".to_string(),
            installation,
            definition,
            required_env: vec!["API_KEY".to_string()],
            enabled_by_default: true,
            shared: false,
        };
        HashMap::from([
            (
                "npm".to_string(),
                server(
                    "npm",
                    McpInstallation::Npm {
                        package: "context7".to_string(),
                        version: Some("1.0.0".to_string()),
                    },
                    McpServerDefinition::Command {
                        command: "node".to_string(),
                        // "8931" is a STRING here (args is `Vec<String>`) and
                        // deliberately looks like a number: a list widget that
                        // guesses element types turns it into an integer and
                        // the next load fails to deserialize. See
                        // `every_row_round_trips_through_its_widget`.
                        args: vec!["--port".to_string(), "8931".to_string()],
                        env: HashMap::from([("PORT".to_string(), "3000".to_string())]),
                    },
                ),
            ),
            (
                "python".to_string(),
                server(
                    "python",
                    McpInstallation::Python {
                        package: "mcp-server".to_string(),
                        version: Some("2.0.0".to_string()),
                    },
                    McpServerDefinition::Json {
                        config: serde_json::json!({ "type": "sse", "url": "http://localhost" }),
                    },
                ),
            ),
            (
                "git".to_string(),
                server(
                    "git",
                    McpInstallation::Git {
                        url: "https://example.invalid/mcp.git".to_string(),
                        branch: Some("main".to_string()),
                        install_command: Some("just build".to_string()),
                    },
                    McpServerDefinition::Command {
                        command: "./mcp".to_string(),
                        args: vec![],
                        env: HashMap::new(),
                    },
                ),
            ),
            (
                "preinstalled".to_string(),
                server(
                    "preinstalled",
                    McpInstallation::PreInstalled,
                    McpServerDefinition::Command {
                        command: "mcp".to_string(),
                        args: vec![],
                        env: HashMap::new(),
                    },
                ),
            ),
            (
                "custom".to_string(),
                server(
                    "custom",
                    McpInstallation::Custom {
                        script: "curl -fsSL https://example.invalid/install.sh | sh".to_string(),
                    },
                    McpServerDefinition::Command {
                        command: "mcp".to_string(),
                        args: vec![],
                        env: HashMap::new(),
                    },
                ),
            ),
        ])
    }

    /// Every leaf path in a serialized config, normalised to registry keys,
    /// mapped to the values observed at that path.
    fn schema_leaves() -> BTreeMap<String, Vec<toml::Value>> {
        let config = fully_populated();
        let value = toml::Value::try_from(&config).expect("config serializes to TOML");
        let mut out = BTreeMap::new();
        walk(&value, "", &mut out);
        out
    }

    fn walk(value: &toml::Value, path: &str, out: &mut BTreeMap<String, Vec<toml::Value>>) {
        if !path.is_empty() && is_opaque_root(path) {
            out.entry(path.to_string()).or_default().push(value.clone());
            return;
        }
        match value {
            toml::Value::Table(table) => {
                for (key, child) in table {
                    walk(child, &child_key(path, key), out);
                }
            }
            leaf => out.entry(path.to_string()).or_default().push(leaf.clone()),
        }
    }

    #[test]
    fn every_leaf_has_an_entry() {
        let missing: Vec<String> = schema_leaves()
            .keys()
            .filter(|key| !is_external(key))
            .filter(|key| entry(key).is_none())
            .cloned()
            .collect();

        assert!(
            missing.is_empty(),
            "config schema leaves with no CONFIG_REGISTRY entry:\n  {}\n\n\
             Add an Entry::Row for each (it is a user preference) or an \
             Entry::Hidden with a written `why` (it is internal state) in \
             crates/ainb-core/src/config/registry.rs.",
            missing.join("\n  ")
        );
    }

    #[test]
    fn every_entry_names_a_real_leaf() {
        let leaves = schema_leaves();
        let stale: Vec<&str> = CONFIG_REGISTRY
            .iter()
            .map(Entry::key)
            .filter(|key| !is_external(key))
            .filter(|key| !leaves.contains_key(*key))
            .collect();

        assert!(
            stale.is_empty(),
            "CONFIG_REGISTRY entries whose key is not in the config schema \
             (typo, or the field was removed):\n  {}",
            stale.join("\n  ")
        );
    }

    /// Every settings category must be reachable. A category with a label, an
    /// icon and no row is a menu entry nothing can ever open — the same drift
    /// in the other direction from `every_leaf_has_an_entry`.
    #[test]
    fn every_category_has_at_least_one_row() {
        let empty: Vec<ConfigCategory> = ConfigCategory::all()
            .into_iter()
            .filter(|category| !rows().any(|row| row.category == *category))
            .collect();
        assert!(
            empty.is_empty(),
            "these categories have no rows and can never be opened: {empty:?}\n\
             Give each one a row, or drop the variant."
        );
    }

    #[test]
    fn registry_keys_are_unique() {
        let mut seen = BTreeMap::new();
        for entry in CONFIG_REGISTRY {
            let count: &mut usize = seen.entry(entry.key()).or_default();
            *count += 1;
        }
        let dupes: Vec<&str> = seen.iter().filter(|(_, n)| **n > 1).map(|(k, _)| *k).collect();
        assert!(dupes.is_empty(), "duplicate registry keys: {dupes:?}");
    }

    #[test]
    fn hidden_entries_explain_themselves() {
        for entry in CONFIG_REGISTRY {
            if let Entry::Hidden { key, why } = entry {
                assert!(!why.trim().is_empty(), "hidden entry '{key}' has no reason");
            }
        }
    }

    #[test]
    fn rows_carry_a_label_and_one_line_of_help() {
        for row in rows() {
            assert!(
                !row.label.trim().is_empty(),
                "row '{}' has no label",
                row.key
            );
            assert!(!row.help.trim().is_empty(), "row '{}' has no help", row.key);
            assert!(
                !row.help.contains('\n'),
                "row '{}' help must be one line",
                row.key
            );
            if let RowKind::Choice(options) = row.kind {
                assert!(
                    !options.is_empty(),
                    "choice row '{}' has no options",
                    row.key
                );
            }
            if let RowKind::Number { min, max } = row.kind {
                assert!(min <= max, "row '{}' has an inverted range", row.key);
            }
        }
    }

    #[test]
    fn choice_options_match_serde() {
        let leaves = schema_leaves();
        for row in rows() {
            let RowKind::Choice(options) = row.kind else {
                continue;
            };
            let Some(observed) = leaves.get(row.key) else {
                continue;
            };
            for value in observed {
                let rendered = scalar_text(value);
                assert!(
                    options.contains(&rendered.as_str()),
                    "'{}' serializes as '{rendered}', which is not in its choice list {options:?}",
                    row.key
                );
            }
        }
    }

    /// Each match below is exhaustive, so adding an enum variant fails to
    /// compile here, which is the prompt to widen the matching choice list.
    #[test]
    fn choice_lists_cover_every_enum_variant() {
        fn token<T: serde::Serialize>(value: &T) -> String {
            match toml::Value::try_from(value).expect("enum serializes") {
                toml::Value::String(s) => s,
                other => panic!("expected a string tag, got {other}"),
            }
        }
        fn assert_covered<T: serde::Serialize>(key: &str, variants: &[T]) {
            let RowKind::Choice(options) = row(key).expect("row exists").kind else {
                panic!("'{key}' is not a choice row");
            };
            for variant in variants {
                let rendered = token(variant);
                assert!(
                    options.contains(&rendered.as_str()),
                    "'{key}' choice list {options:?} is missing '{rendered}'"
                );
            }
            assert_eq!(
                options.len(),
                variants.len(),
                "'{key}' choice list {options:?} does not match the variant count"
            );
        }

        let cli = [
            CliProvider::Claude,
            CliProvider::Codex,
            CliProvider::Gemini,
            CliProvider::Copilot,
        ];
        for variant in &cli {
            match variant {
                CliProvider::Claude
                | CliProvider::Codex
                | CliProvider::Gemini
                | CliProvider::Copilot => {}
            }
        }
        assert_covered("authentication.cli_provider", &cli);

        let claude = [
            ClaudeAuthProvider::SystemAuth,
            ClaudeAuthProvider::ApiKey,
            ClaudeAuthProvider::AmazonBedrock,
            ClaudeAuthProvider::GoogleVertex,
            ClaudeAuthProvider::AzureFoundry,
            ClaudeAuthProvider::GlmZai,
            ClaudeAuthProvider::LlmGateway,
        ];
        for variant in &claude {
            match variant {
                ClaudeAuthProvider::SystemAuth
                | ClaudeAuthProvider::ApiKey
                | ClaudeAuthProvider::AmazonBedrock
                | ClaudeAuthProvider::GoogleVertex
                | ClaudeAuthProvider::AzureFoundry
                | ClaudeAuthProvider::GlmZai
                | ClaudeAuthProvider::LlmGateway => {}
            }
        }
        assert_covered("authentication.claude_provider", &claude);

        let plans = [
            UsagePlanId::ClaudePro,
            UsagePlanId::ClaudeMax,
            UsagePlanId::ClaudeMax5x,
            UsagePlanId::CursorPro,
            UsagePlanId::Custom,
            UsagePlanId::None,
        ];
        for variant in &plans {
            match variant {
                UsagePlanId::ClaudePro
                | UsagePlanId::ClaudeMax
                | UsagePlanId::ClaudeMax5x
                | UsagePlanId::CursorPro
                | UsagePlanId::Custom
                | UsagePlanId::None => {}
            }
        }
        assert_covered("usage.plan.id", &plans);

        let providers = [
            UsagePlanProvider::All,
            UsagePlanProvider::Claude,
            UsagePlanProvider::Codex,
            UsagePlanProvider::Cursor,
        ];
        for variant in &providers {
            match variant {
                UsagePlanProvider::All
                | UsagePlanProvider::Claude
                | UsagePlanProvider::Codex
                | UsagePlanProvider::Cursor => {}
            }
        }
        assert_covered("usage.plan.provider", &providers);

        let collisions = [
            WorktreeCollisionBehavior::AutoRename,
            WorktreeCollisionBehavior::Error,
        ];
        for variant in &collisions {
            match variant {
                WorktreeCollisionBehavior::AutoRename | WorktreeCollisionBehavior::Error => {}
            }
        }
        assert_covered(
            "workspace_defaults.worktree_collision_behavior",
            &collisions,
        );
    }

    /// The `hangar_daemon.*` rows must name exactly the daemon's own knobs, in
    /// the same shape.
    ///
    /// They cannot be generated: `CONFIG_REGISTRY` is a `static` of const
    /// values and `DAEMON_CONFIG_REGISTRY` cannot be iterated into one. So they
    /// are hand-mirrored and pinned here instead. Adding a daemon knob without
    /// adding a row leaves it unreachable from the settings screen and from
    /// `ainb config set`, which is the drift this whole registry exists to
    /// stop, one backend over.
    #[test]
    fn hangar_daemon_rows_match_the_daemon_registry() {
        use ainb_hangar_core::daemon_config::{ConfigKind, DAEMON_CONFIG_REGISTRY};

        let mirrored: Vec<&str> = CONFIG_REGISTRY
            .iter()
            .filter_map(|entry| hangar_daemon_key(entry.key()))
            .collect();
        let declared: Vec<&str> = DAEMON_CONFIG_REGISTRY.iter().map(|d| d.key).collect();
        assert_eq!(
            mirrored, declared,
            "the hangar_daemon.* rows have drifted from DAEMON_CONFIG_REGISTRY"
        );

        for descriptor in DAEMON_CONFIG_REGISTRY {
            let key = format!("{HANGAR_DAEMON_PREFIX}{}", descriptor.key);
            let row = row(&key).unwrap_or_else(|| panic!("{key} is registered"));
            match descriptor.kind {
                ConfigKind::Bool => assert_eq!(
                    row.kind,
                    RowKind::Bool,
                    "{key} is a bool in the daemon registry"
                ),
                ConfigKind::Int { min, max, .. } => assert_eq!(
                    row.kind,
                    RowKind::Number { min, max },
                    "{key} must carry the daemon's own range"
                ),
                ConfigKind::Enum { variants } => match row.kind {
                    RowKind::Choice(options) => assert_eq!(
                        options, variants,
                        "{key} must offer the daemon's own variants"
                    ),
                    other => panic!("{key} must be a choice row, got {other:?}"),
                },
            }
            // The mirrored default has to be one the daemon would itself accept,
            // or the row seeds with a value the store would reject on save.
            descriptor
                .validate(descriptor.default)
                .unwrap_or_else(|why| panic!("{key} default is invalid: {why}"));
        }
    }

    /// A `hangar_daemon.*` key is walk-exempt: it is not in config.toml at all.
    #[test]
    fn hangar_daemon_keys_are_external() {
        assert!(is_external("hangar_daemon.autostandup.enabled"));
        assert_eq!(
            hangar_daemon_key("hangar_daemon.card_agent.default"),
            Some("card_agent.default")
        );
        assert_eq!(hangar_daemon_key("docker.timeout"), None);
    }

    // --- key normalisation ---

    #[test]
    fn map_keys_normalise_to_a_star() {
        assert_eq!(
            registry_key("mcp_servers.context7.shared"),
            "mcp_servers.*.shared"
        );
        assert_eq!(
            registry_key("container_templates.node.config.environment.NODE_ENV"),
            "container_templates.*.config.environment.*"
        );
        assert_eq!(
            registry_key("usage.model_aliases.claude-opus-5"),
            "usage.model_aliases.*"
        );
        assert_eq!(
            registry_key("fleet.cost.session_overrides.abc"),
            "fleet.cost.session_overrides.*"
        );
    }

    #[test]
    fn plugin_fields_survive_normalisation_but_plugin_tables_collapse() {
        assert_eq!(registry_key("plugins.enabled"), "plugins.enabled");
        assert_eq!(registry_key("plugins.disabled"), "plugins.disabled");
        assert_eq!(registry_key("plugins.learnings.learnings_dir"), "plugins.*");
    }

    #[test]
    fn lookup_accepts_concrete_paths() {
        let shared = row("mcp_servers.context7.shared").expect("resolves through the map key");
        assert_eq!(shared.kind, RowKind::Bool);
        assert!(row("nope.not.a.key").is_none());
    }

    // --- validation ---

    #[test]
    fn validate_rejects_an_unknown_key_with_a_hint() {
        let err = validate("docker.timout", "30").unwrap_err().to_string();
        assert!(err.contains("Unknown config key 'docker.timout'"), "{err}");
        assert!(err.contains("docker.timeout"), "{err}");
    }

    #[test]
    fn validate_rejects_an_out_of_range_number() {
        let err = validate("usage.plan.reset_day", "40").unwrap_err().to_string();
        assert!(err.contains("between 1 and 31"), "{err}");
        assert_eq!(
            validate("usage.plan.reset_day", "14").unwrap(),
            toml::Value::Integer(14)
        );
    }

    #[test]
    fn validate_rejects_a_non_numeric_number() {
        let err = validate("docker.timeout", "soon").unwrap_err().to_string();
        assert!(err.contains("whole number"), "{err}");
    }

    #[test]
    fn validate_rejects_an_unlisted_choice() {
        let err = validate("ui_preferences.theme", "solarized").unwrap_err().to_string();
        assert!(err.contains("dark, light"), "{err}");
        assert_eq!(
            validate("ui_preferences.theme", "light").unwrap(),
            toml::Value::String("light".to_string())
        );
    }

    #[test]
    fn validate_rejects_a_non_boolean() {
        let err = validate("mcp_pool.enabled", "yes").unwrap_err().to_string();
        assert!(err.contains("true/false"), "{err}");
        assert_eq!(
            validate("mcp_pool.enabled", "FALSE").unwrap(),
            toml::Value::Boolean(false)
        );
    }

    /// #7. Per-plugin keys must stay settable: their schema is the plugin's own
    /// `[[config]]` manifest, so the registry describes the table as opaque
    /// rather than describing its contents.
    #[test]
    fn a_key_below_an_opaque_root_is_settable() {
        assert_eq!(
            validate("plugins.learnings.learnings_dir", "~/kb").unwrap(),
            toml::Value::String("~/kb".to_string())
        );
        assert_eq!(
            validate("plugins.burndown.daily_cap", "20").unwrap(),
            toml::Value::Integer(20)
        );
        // The opaque ROOT itself is still refused — that is a structured value.
        let err = validate("mcp_servers.ctx.definition.config", "{}").unwrap_err().to_string();
        assert!(err.contains("ainb config edit"), "{err}");
        // And a genuinely unknown key is still a typo.
        assert!(validate("plugins", "x").is_err());
    }

    #[test]
    fn validate_rejects_a_structured_value() {
        let err = validate("mcp_servers.ctx.definition.config", "{}").unwrap_err().to_string();
        assert!(err.contains("ainb config edit"), "{err}");
    }

    #[test]
    fn validate_splits_a_list_and_keeps_scalar_types() {
        let value = validate("container_templates.node.config.ports", "3000, 5173").unwrap();
        assert_eq!(
            value,
            toml::Value::Array(vec![toml::Value::Integer(3000), toml::Value::Integer(5173)])
        );
    }

    #[test]
    fn validate_accepts_a_float_in_range_and_rejects_one_outside() {
        assert_eq!(
            validate("fleet.cost.session_usd", "5.5").unwrap(),
            toml::Value::Float(5.5)
        );
        let err = validate("fleet.cost.session_usd", "-1").unwrap_err().to_string();
        assert!(err.contains("between"), "{err}");
    }

    #[test]
    fn validate_accepts_hidden_leaves_without_a_type() {
        // Internal state is still a real key; refusing it would remove the
        // only escape hatch for a wizard-owned value.
        assert_eq!(
            validate("ui_preferences.home_sidebar_width", "40").unwrap(),
            toml::Value::Integer(40)
        );
    }

    #[test]
    fn set_validated_writes_through_a_nested_path() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        set_validated(&mut root, "mcp_pool.idle_grace_secs", "120").unwrap();
        assert_eq!(
            navigate_toml(&root, "mcp_pool.idle_grace_secs").unwrap(),
            &toml::Value::Integer(120)
        );

        // A rejected value leaves the tree untouched.
        assert!(set_validated(&mut root, "mcp_pool.idle_grace_secs", "-5").is_err());
        assert_eq!(
            navigate_toml(&root, "mcp_pool.idle_grace_secs").unwrap(),
            &toml::Value::Integer(120)
        );
    }

    // --- settings-screen conversion ---

    #[test]
    fn to_setting_seeds_from_the_current_value() {
        let config = toml::Value::try_from(fully_populated()).unwrap();
        let theme = row("ui_preferences.theme").unwrap();
        let current = navigate_toml(&config, "ui_preferences.theme").unwrap();
        let setting = theme.to_setting(Some(current));

        assert_eq!(setting.key, "ui_preferences.theme");
        assert_eq!(setting.label, "Theme");
        assert_eq!(setting.description, theme.help);
        match setting.value {
            ConfigValue::Choice(options, idx) => {
                assert_eq!(options[idx], "light");
            }
            other => panic!("expected a choice widget, got {other:?}"),
        }
    }

    #[test]
    fn to_setting_renders_each_kind() {
        let config = toml::Value::try_from(fully_populated()).unwrap();
        let value_at = |key: &str| navigate_toml(&config, key).ok().cloned();

        let bool_row = row("mcp_pool.enabled").unwrap();
        assert!(matches!(
            bool_row.to_value(value_at("mcp_pool.enabled").as_ref()),
            ConfigValue::Bool(true)
        ));

        let number_row = row("docker.timeout").unwrap();
        assert!(matches!(
            number_row.to_value(value_at("docker.timeout").as_ref()),
            ConfigValue::Number(60)
        ));

        let list_row = row("plugins.disabled").unwrap();
        match list_row.to_value(value_at("plugins.disabled").as_ref()) {
            ConfigValue::Text(text) => assert_eq!(text, "burndown"),
            other => panic!("expected text, got {other:?}"),
        }

        // Floats keep their precision rather than being rounded into an int.
        let float_row = row("usage.currency.usd_rate").unwrap();
        match float_row.to_value(value_at("usage.currency.usd_rate").as_ref()) {
            ConfigValue::Text(text) => assert_eq!(text, "0.78"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn secret_rows_produce_a_masked_widget() {
        for key in [
            "fleet.bridge.telegram.token",
            "fleet.bridge.slack.bot_token",
            "fleet.bridge.slack.app_token",
            "fleet.bridge.discord.token",
            "skills.api_key",
        ] {
            let secret = row(key).unwrap_or_else(|| panic!("{key} is registered"));
            assert_eq!(secret.kind, RowKind::Secret, "{key} must be a secret");
            let value = secret.to_value(Some(&toml::Value::String("xoxb-abcdef123".to_string())));
            match value {
                ConfigValue::Secret(_) => {
                    assert!(
                        !value.display().contains("abcdef123"),
                        "{key} leaked its value"
                    );
                }
                other => panic!("expected a secret widget for {key}, got {other:?}"),
            }
        }
    }

    // --- review findings #6 / #7: what a widget's raw string writes back ---

    /// Every leaf must survive the trip through its widget unchanged.
    ///
    /// `scalar_text` renders a value for editing and `validate` parses it back;
    /// if those two disagree, confirming a row without touching it rewrites the
    /// value into a different TYPE, and the next load fails to deserialize.
    /// Found `mcp_servers.*.definition.args` (a `Vec<String>`) turning
    /// `["--port", "8931"]` into `["--port", 8931]`.
    #[test]
    fn every_row_round_trips_through_its_widget() {
        let mut broken = Vec::new();
        for (key, observed) in schema_leaves() {
            let Some(row) = row(&key) else { continue };
            if matches!(row.kind, RowKind::Opaque) {
                continue;
            }
            for value in observed {
                let rendered = scalar_text(&value);
                let Ok(parsed) = validate(&key, &rendered) else {
                    broken.push(format!("{key}: '{rendered}' does not validate"));
                    continue;
                };
                if parsed != value {
                    broken.push(format!("{key}: {value} -> '{rendered}' -> {parsed}"));
                }
            }
        }
        assert!(
            broken.is_empty(),
            "these rows change their value's type on a no-op edit:\n  {}",
            broken.join("\n  ")
        );
    }

    /// `OPTIONAL_KEYS` must name exactly the leaves serde leaves absent.
    ///
    /// Derived, not trusted: drop each leaf from a fully-populated config,
    /// deserialize, re-serialize, and see whether it comes back. A field with a
    /// serde default reappears; an `Option` stays gone. Clearing the widget of
    /// the latter has to REMOVE the key — storing `""` gives `Some("")`, which
    /// is how `docker.host` ends up as an empty endpoint Docker then tries to
    /// dial.
    #[test]
    fn optional_keys_match_the_schema() {
        let full = toml::Value::try_from(fully_populated()).unwrap();

        let mut derived: Vec<String> = Vec::new();
        for (key, _) in schema_leaves() {
            if row(&key).is_none() || is_external(&key) {
                continue;
            }
            let Some(concrete) = first_concrete_path(&full, &key) else {
                continue;
            };
            let mut trimmed = full.clone();
            if !remove_at(&mut trimmed, &concrete) {
                continue;
            }
            let Ok(parsed) = trimmed.try_into::<AppConfig>() else {
                continue; // Required by the schema; not optional.
            };
            let reserialized = toml::Value::try_from(parsed).unwrap();
            if navigate_toml(&reserialized, &concrete).is_err() {
                derived.push(key);
            }
        }
        derived.sort();
        derived.dedup();

        let mut declared: Vec<String> = OPTIONAL_KEYS.iter().map(|k| (*k).to_string()).collect();
        declared.sort();

        assert_eq!(
            declared, derived,
            "OPTIONAL_KEYS has drifted from the schema.\n  declared: {declared:?}\n  derived:  {derived:?}"
        );
    }

    /// Resolve a registry key's `*` segments against a real config, taking the
    /// first instance of each map.
    fn first_concrete_path(seed: &toml::Value, key: &str) -> Option<String> {
        let mut path = String::new();
        let mut node = seed;
        for segment in parse_dot_key(key) {
            let table = node.as_table()?;
            let name = if segment == "*" {
                table.keys().next()?.clone()
            } else {
                segment.clone()
            };
            node = table.get(&name)?;
            path = if path.is_empty() {
                name
            } else {
                format!("{path}.{name}")
            };
        }
        Some(path)
    }

    #[test]
    fn clearing_an_optional_row_removes_the_key() {
        let mut root: toml::Value =
            toml::from_str("[docker]\nhost = \"tcp://1.2.3.4:2376\"\n").unwrap();
        set_validated(&mut root, "docker.host", "").unwrap();
        assert!(
            navigate_toml(&root, "docker.host").is_err(),
            "clearing an Option<String> must remove the key, not store \"\": {root}"
        );
        // And the config still loads, with the field back to None.
        let parsed: AppConfig = root.try_into().unwrap();
        assert_eq!(parsed.docker.host, None);
    }

    #[test]
    fn clearing_a_defaulted_row_keeps_the_empty_value() {
        // `branch_prefix` is a plain `String` with a serde default. Removing it
        // would silently restore "agents/" instead of honouring the edit, so an
        // empty value is stored as an empty value.
        let mut root: toml::Value = toml::from_str("[workspace_defaults]\n").unwrap();
        set_validated(&mut root, "workspace_defaults.branch_prefix", "").unwrap();
        assert_eq!(
            navigate_toml(&root, "workspace_defaults.branch_prefix").unwrap(),
            &toml::Value::String(String::new())
        );
    }

    #[test]
    fn missing_value_seeds_an_empty_widget() {
        let host = row("docker.host").unwrap();
        assert!(matches!(host.to_value(None), ConfigValue::Text(text) if text.is_empty()));
    }
}
