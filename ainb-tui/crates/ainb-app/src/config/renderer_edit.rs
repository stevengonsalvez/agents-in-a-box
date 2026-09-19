//! Which settings rows a renderer may edit (#1224).
//!
//! A DOM renderer is script-reachable, so an edit it sends is judged twice:
//! against a deny list of rows whose value reaches a program the host runs
//! (an editor it spawns, a container command, an MCP server's command, a
//! socket it connects to) and then against an allow list of the rows the
//! desktop's settings page draws. A row on neither list is refused too: the
//! allow list is what the page edits, not everything the deny list missed.
//!
//! The terminal is not a renderer in this sense: its keys are read by the host
//! from a tty, so the wizard and the popup keep editing every row. What this
//! module gates is a row edit that arrived as a renderer intent, whether as
//! `config.set_row` by name or as the key sequence that opens the popup and
//! confirms it ([`crate::app::AppState::remote_command_refusal`]).
//!
//! Both lists are spelled as registry keys, with `*` for a map segment, and
//! `tests/fixtures/renderer_editable_rows.txt` commits them so the settings
//! page's own copy of the policy (`ainb-desktop/ui/src/settings.ts`) is diffed
//! against this one.

use super::registry;

/// Rows whose value reaches a spawn, a mount or a connection the host makes,
/// each with where. Refused to a renderer whatever the allow list says.
pub const DENIED: &[(&str, &str)] = &[
    (
        "ui_preferences.preferred_editor",
        "spawned by Effect::OpenEditor",
    ),
    ("acp.adapters.*.command", "spawned as the ACP adapter"),
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
        "container_templates.*.config.image_source.path",
        "the image build context",
    ),
    (
        "container_templates.*.config.image_source.build_args.*",
        "the image build arguments",
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
        "mcp_servers.*.installation.install_command",
        "run to install the server",
    ),
    (
        "mcp_servers.*.installation.script",
        "run to install the server",
    ),
    (
        "mcp_servers.*.installation.package",
        "installed with the package manager",
    ),
    (
        "mcp_servers.*.installation.url",
        "fetched to install the server",
    ),
    (
        "mcp_servers.*.installation.branch",
        "fetched to install the server",
    ),
    ("docker.host", "the Docker socket the host connects to"),
    ("fleet.terminal", "the terminal application the host opens"),
    (
        "hangar_daemon.card_agent.default",
        "the agent the daemon spawns for a card",
    ),
    ("plugins.enabled", "which plugin binaries the host loads"),
    ("plugins.disabled", "which plugin binaries the host loads"),
    ("plugins.*", "a plugin's own configuration"),
    ("presets.file", "a file the host reads presets from"),
    ("usage_client.cache_db", "a database path the host opens"),
    ("web.listen", "a socket the web server binds"),
    (
        "web.insecure_bind",
        "lets the web server bind beyond loopback",
    ),
    (
        "skills.catalog_release",
        "the release the skill catalog is fetched from",
    ),
];

/// Rows the desktop's settings page draws and a renderer may edit, once the
/// deny list has passed. A trailing `.` allows every row under the prefix.
pub const ALLOWED: &[&str] = &[
    "general.",
    "authentication.",
    "workspace_defaults.",
    "ui_preferences.",
    "ui.",
    "docker.timeout",
    "default_container_template",
    "container_templates.*.name",
    "container_templates.*.description",
    "container_templates.*.config.working_dir",
    "container_templates.*.config.user",
    "container_templates.*.config.memory_limit",
    "container_templates.*.config.cpu_limit",
    "container_templates.*.config.ports",
    "container_templates.*.required_env",
    "container_templates.*.default_mcp_servers",
    "mcp_servers.*.name",
    "mcp_servers.*.description",
    "mcp_servers.*.enabled_by_default",
    "mcp_servers.*.shared",
    "mcp_servers.*.required_env",
    "mcp_servers.*.installation.type",
    "mcp_servers.*.installation.version",
    "mcp_servers.*.definition.type",
    "fleet.",
    "mcp_pool.",
    "usage_client.headroom_port",
    "usage_client.fetch_timeout_secs",
    "usage_client.codex_ttl_secs",
    "daemons.",
    "notifyd.",
    "web.read_only",
    "acp.adapters.*.permission_mode",
    "skills.api_key",
    "session_reader.",
    "hangar_daemon.autostandup.",
    "hangar_daemon.workspace.",
];

/// The refusal a renderer's edit of `key` gets.
pub const DENIED_REASON: &str =
    "its value reaches a program the host runs, so the window may not set it";
/// The refusal a renderer's edit of a row off the allow list gets.
pub const NOT_DRAWN_REASON: &str = "the window's settings page does not edit it";

/// Whether the registry key `key` matches `pattern`: exactly, or under a
/// prefix that ends in `.`.
fn matches(pattern: &str, key: &str) -> bool {
    match pattern.strip_suffix('.') {
        Some(prefix) => key.starts_with(prefix) && key[prefix.len()..].starts_with('.'),
        None => pattern == key,
    }
}

/// Why a renderer may not edit the row `key`, or `None` when it may.
///
/// `key` is a concrete dotted key as a row carries it; map segments collapse
/// to `*` through the registry, so `mcp_servers.github.definition.command`
/// meets `mcp_servers.*.definition.command`.
#[must_use]
pub fn refusal(key: &str) -> Option<&'static str> {
    let key = registry::registry_key(key);
    if DENIED.iter().any(|(pattern, _)| matches(pattern, &key)) {
        return Some(DENIED_REASON);
    }
    if ALLOWED.iter().any(|pattern| matches(pattern, &key)) {
        return None;
    }
    Some(NOT_DRAWN_REASON)
}

/// Every registry row with the verdict a renderer's edit of it gets, one line
/// each: `allow <key>` or `deny <key>`. Committed as a fixture so the page's
/// own copy of the policy is diffed against this one.
#[must_use]
pub fn verdicts() -> String {
    let mut lines: Vec<String> = registry::rows()
        .map(|row| {
            let verdict = if refusal(row.key).is_none() {
                "allow"
            } else {
                "deny"
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
        assert_eq!(refusal("acp.adapters.claude.permission_mode"), None);
    }

    #[test]
    fn the_deny_list_wins_over_a_prefix_on_the_allow_list() {
        assert_eq!(
            refusal("ui_preferences.preferred_editor"),
            Some(DENIED_REASON)
        );
        assert_eq!(refusal("ui_preferences.theme"), None);
        assert_eq!(refusal("fleet.terminal"), Some(DENIED_REASON));
        assert_eq!(refusal("fleet.idle_min"), None);
    }

    #[test]
    fn a_row_on_neither_list_is_refused() {
        assert_eq!(refusal("usage.plan.id"), Some(NOT_DRAWN_REASON));
        assert_eq!(refusal("no.such.row"), Some(NOT_DRAWN_REASON));
    }

    #[test]
    fn a_prefix_does_not_match_a_longer_name() {
        assert!(!matches("ui.", "ui_preferences.theme"));
        assert!(matches("ui.", "ui.tick_rate_ms"));
    }
}
