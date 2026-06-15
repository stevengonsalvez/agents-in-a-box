// ABOUTME: Shared MCP server pool — one MCP child process per server name,
// multiplexed across all host (tmux) sessions via unix sockets.
//
// Topology:
//   claude (session N) ──stdio── `ainb mcp proxy <sock>` shim ──unix sock──┐
//                                                                          │
//   `ainb mcp daemon` ── per-server SocketProxy (mux) ── 1× MCP child ◄────┘
//
// The daemon is standalone (survives the TUI); children spawn lazily on the
// first client connect and are reaped `idle_grace_secs` after the last
// client detaches. The mux rewrites JSON-RPC request ids per client, caches
// the backend InitializeResult so repeat initializes never hit the child,
// and routes progress notifications by progressToken to the owning session.

pub mod client;
pub mod daemon;
pub mod import;
pub mod install;
pub mod mcp_json;
pub mod mux;
pub mod paths;
pub mod proxy;
pub mod shim;

use crate::config::{AppConfig, McpServerConfig, McpServerDefinition};

/// A server eligible for pooling, with its resolved spawn parameters.
/// Serializable — sessions register servers with a running daemon over the
/// control socket, so the daemon isn't limited to config visible at ITS cwd.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PooledServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Whether a server name is safe to use as a socket filename and as a TOML/
/// JSON config key. Names flow from untrusted sources (a repo's `.mcp.json`
/// keys), so anything that could traverse out of the sockets dir
/// (`..`, `/`) or break out of a config-table key (quotes, dots, control
/// chars) is rejected. Conservative allowlist: ASCII alnum + `_` + `-`,
/// 1–64 chars.
pub fn valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl PooledServer {
    /// Host-resolvability check shared by config eligibility and .mcp.json
    /// import: command on PATH (or an existing absolute path) and no
    /// container-only arg paths.
    pub fn resolvable_on_host(&self) -> bool {
        let cmd_path = std::path::Path::new(&self.command);
        let ok = if cmd_path.is_absolute() {
            cmd_path.exists()
        } else {
            which::which(&self.command).is_ok()
        };
        ok && !self.args.iter().any(|a| a.starts_with("/home/claude-user"))
    }
}

/// Decide which configured MCP servers the pool will share on this host.
///
/// Eligibility: `shared = true` (default), `enabled_by_default = true`, a
/// Command-style definition (JSON definitions are container-oriented), the
/// command resolves on PATH (or is an existing absolute path), and no arg
/// references a container-only path. The built-in defaults point at
/// `/home/claude-user/...` which only exists inside containers — pooling
/// those on the host would spawn children that instantly crash.
pub fn pooled_servers(config: &AppConfig) -> Vec<PooledServer> {
    let mut out: Vec<PooledServer> =
        config.mcp_servers.values().filter_map(|s| eligible(s)).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn eligible(server: &McpServerConfig) -> Option<PooledServer> {
    if !server.shared || !server.enabled_by_default {
        return None;
    }
    if !valid_server_name(&server.name) {
        tracing::warn!(
            "mcp_pool: skipping server with unsafe name {:?}",
            server.name
        );
        return None;
    }
    let McpServerDefinition::Command { command, args, env } = &server.definition else {
        return None;
    };

    let candidate = PooledServer {
        name: server.name.clone(),
        command: command.clone(),
        args: args.clone(),
        env: env.clone(),
    };
    if !candidate.resolvable_on_host() {
        tracing::debug!(
            "mcp_pool: skipping '{}' — not resolvable on host",
            server.name
        );
        return None;
    }
    Some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cmd_server(name: &str, command: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            description: String::new(),
            installation: crate::config::McpInstallation::PreInstalled,
            definition: McpServerDefinition::Command {
                command: command.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
                env: HashMap::new(),
            },
            required_env: vec![],
            enabled_by_default: true,
            shared: true,
        }
    }

    #[test]
    fn pooled_servers_filters_opt_out_and_container_paths() {
        let mut config = AppConfig::default();
        config.mcp_servers.clear();

        // `sh` resolves everywhere on unix.
        config.mcp_servers.insert("ok".into(), cmd_server("ok", "sh", &["-c", "true"]));

        let mut opted_out = cmd_server("nope", "sh", &[]);
        opted_out.shared = false;
        config.mcp_servers.insert("nope".into(), opted_out);

        config.mcp_servers.insert(
            "container".into(),
            cmd_server(
                "container",
                "node",
                &["/home/claude-user/.npm-global/lib/x.js"],
            ),
        );

        config.mcp_servers.insert(
            "missing".into(),
            cmd_server("missing", "definitely-not-a-binary-xyz", &[]),
        );

        let pooled = pooled_servers(&config);
        assert_eq!(pooled.len(), 1, "{pooled:?}");
        assert_eq!(pooled[0].name, "ok");
    }

    #[test]
    fn valid_server_name_accepts_safe_rejects_traversal_and_keys() {
        for ok in ["context7", "my-server", "srv_1", "A", &"x".repeat(64)] {
            assert!(valid_server_name(ok), "should accept {ok:?}");
        }
        for bad in [
            "",              // empty
            &"x".repeat(65), // too long
            "../etc",        // traversal
            "a/b",           // slash
            "..",            // parent
            "a.b",           // dotted → nested TOML table
            "a\"b",          // quote → key breakout
            "a b",           // space
            "a\nb",          // control char
            "srv]",          // bracket
        ] {
            assert!(!valid_server_name(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn unsafe_name_rejected_from_config_and_socket_path() {
        // Config eligibility drops an unsafe name…
        let mut config = AppConfig::default();
        config.mcp_servers.clear();
        config.mcp_servers.insert("../evil".into(), cmd_server("../evil", "sh", &[]));
        assert!(
            pooled_servers(&config).is_empty(),
            "unsafe config name must not pool"
        );

        // …and server_socket refuses to build a traversal path.
        assert!(crate::mcp_pool::paths::server_socket("../../etc/x").is_err());
        assert!(crate::mcp_pool::paths::server_socket("context7").is_ok());
    }
}
