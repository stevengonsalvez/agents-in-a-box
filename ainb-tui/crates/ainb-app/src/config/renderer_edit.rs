//! Which settings rows a renderer may edit (#1224).
//!
//! A DOM renderer is script-reachable, so an edit it sends is judged against
//! two exact lists of registry keys and refused otherwise: [`DENIED`], the
//! rows whose value reaches a program the host runs, a socket it binds or
//! connects to, a mount, an approval gate, or the bridge's identity; and
//! [`ALLOWED`], the rows the desktop's settings page edits. Deny by default: a
//! row on neither list is refused, and `tests/config_renderer_edits.rs` walks
//! the whole config schema so a new row fails until it is classified here.
//! There are no prefixes; each entry names one row, with `*` for a map
//! segment only. Hidden registry entries (no row on the screen) are on
//! neither list: an edit of one finds no row and is refused as unclassified.
//!
//! Secret rows are not renderer-settable in this slice, literal or reference
//! alike: the page shows presence and source, and the write path (the popup,
//! the keychain prompt, the API key prompt) is refused from a renderer with
//! [`SECRET_REASON`]. A secret entry flow with a keychain path is its own
//! slice.
//!
//! The terminal is not a renderer in this sense: its keys are read by the host
//! from a tty, so the wizard and the popup keep editing every row. What this
//! module gates is a row edit that arrived as a renderer intent, whether as
//! `config.set_row` by name or as the key sequence that opens the popup and
//! confirms it ([`crate::app::AppState::remote_command_refusal`]).
//!
//! `tests/fixtures/renderer_editable_rows.txt` commits the verdict per row so
//! the settings page's own copy of the policy
//! (`ainb-desktop/ui/src/settings.ts`) is diffed against this one.

use super::registry;

/// Rows a renderer may never set, each with why.
pub const DENIED: &[(&str, &str)] = &[
    ("presets.file", "a file the host reads presets from"),
    (
        "general.skill_install_real_homes",
        "lets a skill install write the tools' real homes",
    ),
    (
        "ui_preferences.preferred_editor",
        "spawned by Effect::OpenEditor",
    ),
    (
        "container_templates.*.config.image_source.type",
        "what image is built or pulled",
    ),
    (
        "container_templates.*.config.image_source.name",
        "what image is built or pulled",
    ),
    (
        "container_templates.*.config.image_source.path",
        "the image build context",
    ),
    (
        "container_templates.*.config.image_source.base_image",
        "what image is built or pulled",
    ),
    (
        "container_templates.*.config.image_source.build_args.*",
        "the image build arguments",
    ),
    (
        "container_templates.*.config.command",
        "the container's command",
    ),
    (
        "container_templates.*.config.entrypoint",
        "the container's entrypoint",
    ),
    (
        "container_templates.*.config.environment.*",
        "the container's environment",
    ),
    (
        "container_templates.*.config.user",
        "the user the container runs as",
    ),
    (
        "container_templates.*.config.system_packages",
        "installed into the image",
    ),
    (
        "container_templates.*.config.npm_packages",
        "installed into the image",
    ),
    (
        "container_templates.*.config.python_packages",
        "installed into the image",
    ),
    (
        "container_templates.*.config.ports",
        "ports the container exposes",
    ),
    (
        "container_templates.*.config.volumes",
        "host paths mounted into the container",
    ),
    (
        "container_templates.*.config.mount_ssh",
        "mounts the host's ssh keys",
    ),
    (
        "container_templates.*.config.mount_git_config",
        "mounts the host's git config",
    ),
    (
        "mcp_servers.*.installation.type",
        "how the server is installed",
    ),
    (
        "mcp_servers.*.installation.package",
        "installed with the package manager",
    ),
    (
        "mcp_servers.*.installation.version",
        "which package version is installed",
    ),
    (
        "mcp_servers.*.installation.url",
        "fetched to install the server",
    ),
    (
        "mcp_servers.*.installation.branch",
        "fetched to install the server",
    ),
    (
        "mcp_servers.*.installation.install_command",
        "run to install the server",
    ),
    (
        "mcp_servers.*.installation.script",
        "run to install the server",
    ),
    (
        "mcp_servers.*.definition.type",
        "how the server is spawned or reached",
    ),
    (
        "mcp_servers.*.definition.command",
        "spawned as the MCP server",
    ),
    (
        "mcp_servers.*.definition.args",
        "the MCP server's arguments",
    ),
    (
        "mcp_servers.*.definition.env.*",
        "the MCP server's environment",
    ),
    (
        "mcp_servers.*.definition.config",
        "the MCP server's own configuration",
    ),
    (
        "workspace_defaults.exclude_paths",
        "which paths the repository scan reads",
    ),
    (
        "workspace_defaults.workspace_scan_paths",
        "which paths the repository scan reads",
    ),
    ("docker.host", "the Docker socket the host connects to"),
    ("usage.plan.id", "[usage] is the burndown plugin's file"),
    (
        "usage.plan.monthly_usd",
        "[usage] is the burndown plugin's file",
    ),
    (
        "usage.plan.provider",
        "[usage] is the burndown plugin's file",
    ),
    (
        "usage.plan.reset_day",
        "[usage] is the burndown plugin's file",
    ),
    (
        "usage.currency.code",
        "[usage] is the burndown plugin's file",
    ),
    (
        "usage.currency.symbol",
        "[usage] is the burndown plugin's file",
    ),
    (
        "usage.currency.usd_rate",
        "[usage] is the burndown plugin's file",
    ),
    (
        "usage.model_aliases.*",
        "[usage] is the burndown plugin's file",
    ),
    ("plugins.enabled", "which plugin binaries the host loads"),
    ("plugins.disabled", "which plugin binaries the host loads"),
    ("fleet.terminal", "the terminal application the host opens"),
    ("fleet.transport", "how fleet reaches its sessions"),
    ("fleet.bridge.outbound_enabled", "the bridge's outbound leg"),
    (
        "fleet.bridge.outbound_poll_secs",
        "the bridge's outbound leg",
    ),
    (
        "fleet.bridge.response_timeout",
        "the bridge's answer window",
    ),
    ("fleet.bridge.telegram.token", "the bridge's credential"),
    (
        "fleet.bridge.telegram.user_id",
        "the bridge's only inbound authorization",
    ),
    (
        "fleet.bridge.telegram.default_target",
        "where the bridge delivers",
    ),
    (
        "fleet.bridge.telegram.require_mention_in_groups",
        "the bridge's inbound filter",
    ),
    (
        "fleet.bridge.telegram.response_timeout",
        "the bridge's answer window",
    ),
    ("fleet.bridge.slack.bot_token", "the bridge's credential"),
    ("fleet.bridge.slack.app_token", "the bridge's credential"),
    (
        "fleet.bridge.slack.user_id",
        "the bridge's only inbound authorization",
    ),
    (
        "fleet.bridge.slack.default_target",
        "where the bridge delivers",
    ),
    ("fleet.bridge.slack.listen_mode", "how the bridge listens"),
    (
        "fleet.bridge.slack.response_timeout",
        "the bridge's answer window",
    ),
    ("fleet.bridge.discord.token", "the bridge's credential"),
    (
        "fleet.bridge.discord.user_id",
        "the bridge's only inbound authorization",
    ),
    (
        "fleet.bridge.discord.default_target",
        "where the bridge delivers",
    ),
    (
        "fleet.bridge.discord.channel_id",
        "where the bridge delivers",
    ),
    (
        "fleet.bridge.discord.response_timeout",
        "the bridge's answer window",
    ),
    (
        "usage_client.headroom_port",
        "a local port the usage client connects to",
    ),
    ("usage_client.cache_db", "a database path the host opens"),
    (
        "notifyd.approval_timeout_secs",
        "how long an approval waits before it lapses",
    ),
    ("web.listen", "a socket the web server binds"),
    ("web.read_only", "whether the web server accepts writes"),
    (
        "web.insecure_bind",
        "lets the web server bind beyond loopback",
    ),
    ("acp.adapters.*.command", "spawned as the ACP adapter"),
    ("acp.adapters.*.permission_mode", "the agent approval gate"),
    (
        "skills.catalog_release",
        "the release the skill catalog is fetched from",
    ),
    ("skills.api_key", "a credential"),
    (
        "hangar_daemon.card_agent.default",
        "the agent the daemon spawns for a card",
    ),
];

/// Rows the desktop's settings page edits. Exact keys, `*` for a map segment.
pub const ALLOWED: &[&str] = &[
    "default_container_template",
    "general.syntax_highlight",
    "authentication.cli_provider",
    "authentication.claude_provider",
    "authentication.default_model",
    "authentication.github_method",
    "container_templates.*.name",
    "container_templates.*.description",
    "container_templates.*.config.working_dir",
    "container_templates.*.config.memory_limit",
    "container_templates.*.config.cpu_limit",
    "container_templates.*.required_env",
    "container_templates.*.default_mcp_servers",
    "mcp_servers.*.name",
    "mcp_servers.*.description",
    "mcp_servers.*.required_env",
    "mcp_servers.*.enabled_by_default",
    "mcp_servers.*.shared",
    "workspace_defaults.branch_prefix",
    "workspace_defaults.max_repositories",
    "workspace_defaults.worktree_collision_behavior",
    "workspace_defaults.scan_max_depth",
    "workspace_defaults.scan_cache_ttl_secs",
    "ui_preferences.theme",
    "ui_preferences.show_container_status",
    "ui_preferences.show_git_status",
    "ui_preferences.show_session_menu_bar",
    "ui.tick_rate_ms",
    "ui.app_tick_ms",
    "ui.session_query_limit",
    "ui.session_lookback_hours",
    "ui.attention_err_window_hours",
    "ui.inbox_list_limit",
    "ui.double_click_ms",
    "ui.notice_error_secs",
    "ui.notice_warning_secs",
    "ui.notice_info_secs",
    "docker.timeout",
    "fleet.cost.session_usd",
    "fleet.cost.group_usd",
    "fleet.cost.session_overrides.*",
    "fleet.cost.group_overrides.*",
    "fleet.interview.surface",
    "fleet.idle_min",
    "fleet.enrich",
    "fleet.state_stale_ms",
    "fleet.healthy_state_stale_ms",
    "fleet.tmux_idle_after_secs",
    "mcp_pool.enabled",
    "mcp_pool.idle_grace_secs",
    "mcp_pool.monitor_refresh_secs",
    "mcp_pool.daemon_idle_grace_secs",
    "usage_client.fetch_timeout_secs",
    "usage_client.codex_ttl_secs",
    "daemons.stale_after_ms",
    "daemons.attention_stale_after_ms",
    "notifyd.os_debounce_secs",
    "session_reader.incremental_window_days",
    "hangar_daemon.autostandup.enabled",
    "hangar_daemon.autostandup.stagnant_min",
    "hangar_daemon.autostandup.cooldown_min",
    "hangar_daemon.autostandup.max_concurrent",
    "hangar_daemon.workspace.creation_disabled",
];

/// The refusal a renderer's edit of a denied row gets.
pub const DENIED_REASON: &str =
    "its value reaches something the host runs, binds or trusts, so the window may not set it";
/// The refusal a renderer's edit of an unclassified row gets.
pub const NOT_DRAWN_REASON: &str = "the window's settings page does not edit it";
/// The refusal a renderer's write of a secret row gets, literal or reference.
pub const SECRET_REASON: &str =
    "a secret is set from the terminal, not the window; the page shows only whether it is set";

/// The most characters a renderer's text edit may carry. The desktop's own
/// `Text` intents stop at the same count.
pub const MAX_TEXT_CHARS: usize = 2_000;

/// Why a renderer may not edit the row `key`, or `None` when it may.
///
/// `key` is a concrete dotted key as a row carries it; map segments collapse
/// to `*` through the registry, so `mcp_servers.github.definition.command`
/// meets `mcp_servers.*.definition.command`. A secret row is refused whatever
/// the lists say.
#[must_use]
pub fn refusal(key: &str) -> Option<&'static str> {
    let normalised = registry::registry_key(key);
    if registry::row(key).is_some_and(|row| matches!(row.kind, registry::RowKind::Secret)) {
        return Some(SECRET_REASON);
    }
    if DENIED.iter().any(|(pattern, _)| *pattern == normalised) {
        return Some(DENIED_REASON);
    }
    if ALLOWED.contains(&normalised.as_str()) {
        return None;
    }
    Some(NOT_DRAWN_REASON)
}

/// `text` as a renderer's edit may carry it: control and format characters
/// removed, and no longer than [`MAX_TEXT_CHARS`].
///
/// # Errors
///
/// When the text is over the bound, for a notice; it is never cut, because a
/// value silently shortened is a value the person did not choose.
pub fn clean_text(text: &str) -> Result<String, &'static str> {
    let cleaned: String = text.chars().filter(|c| !c.is_control() && !is_format(*c)).collect();
    if cleaned.chars().count() > MAX_TEXT_CHARS {
        return Err("the value is longer than a settings row takes");
    }
    Ok(cleaned)
}

/// Whether `c` is in Unicode's `Cf` (format) category: bidi overrides and
/// isolates, zero-width joiners, the byte-order mark, tag characters.
fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// Every registry row with the verdict a renderer's edit of it gets, one line
/// each: `allow <key>`, `deny <key>` or `secret <key>`. Committed as a fixture
/// so the page's own copy of the policy is diffed against this one.
#[must_use]
pub fn verdicts() -> String {
    let mut lines: Vec<String> = registry::rows()
        .map(|row| {
            let verdict = match refusal(row.key) {
                None => "allow",
                Some(SECRET_REASON) => "secret",
                Some(_) => "deny",
            };
            format!("{verdict} {}", row.key)
        })
        .collect();
    lines.sort();
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_key_meets_its_pattern() {
        assert_eq!(
            refusal("mcp_servers.github.definition.command"),
            Some(DENIED_REASON)
        );
        assert_eq!(
            refusal("container_templates.default.config.entrypoint"),
            Some(DENIED_REASON)
        );
        assert_eq!(refusal("mcp_servers.github.shared"), None);
    }

    #[test]
    fn there_are_no_prefixes_so_a_sibling_row_is_not_allowed_by_its_neighbour() {
        assert_eq!(refusal("ui_preferences.theme"), None);
        assert_eq!(
            refusal("ui_preferences.preferred_editor"),
            Some(DENIED_REASON)
        );
        assert_eq!(refusal("fleet.idle_min"), None);
        assert_eq!(refusal("fleet.terminal"), Some(DENIED_REASON));
        assert_eq!(
            refusal("fleet.bridge.telegram.user_id"),
            Some(DENIED_REASON)
        );
        assert_eq!(
            refusal("acp.adapters.claude.permission_mode"),
            Some(DENIED_REASON)
        );
        assert_eq!(refusal("ui.no_such_row"), Some(NOT_DRAWN_REASON));
        assert_eq!(refusal("no.such.row"), Some(NOT_DRAWN_REASON));
    }

    #[test]
    fn a_secret_row_is_refused_before_the_lists() {
        assert_eq!(refusal("fleet.bridge.telegram.token"), Some(SECRET_REASON));
        assert_eq!(refusal("skills.api_key"), Some(SECRET_REASON));
    }

    #[test]
    fn text_is_cleaned_and_bounded() {
        assert_eq!(
            clean_text("stag\u{202E}ing\u{200B}\u{0007}"),
            Ok("staging".to_string())
        );
        assert_eq!(
            clean_text(&"a".repeat(MAX_TEXT_CHARS)).map(|t| t.len()),
            Ok(MAX_TEXT_CHARS)
        );
        assert!(clean_text(&"a".repeat(MAX_TEXT_CHARS + 1)).is_err());
    }
}
