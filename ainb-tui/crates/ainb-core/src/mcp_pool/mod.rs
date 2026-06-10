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
pub mod mcp_json;
pub mod mux;
pub mod paths;
pub mod proxy;
pub mod shim;

use crate::config::{AppConfig, McpServerConfig, McpServerDefinition};

/// A server eligible for pooling, with its resolved spawn parameters.
#[derive(Debug, Clone)]
pub struct PooledServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
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
    let mut out: Vec<PooledServer> = config
        .mcp_servers
        .values()
        .filter_map(|s| eligible(s))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn eligible(server: &McpServerConfig) -> Option<PooledServer> {
    if !server.shared || !server.enabled_by_default {
        return None;
    }
    let McpServerDefinition::Command { command, args, env } = &server.definition else {
        return None;
    };

    let cmd_path = std::path::Path::new(command);
    let resolvable = if cmd_path.is_absolute() {
        cmd_path.exists()
    } else {
        which::which(command).is_ok()
    };
    if !resolvable {
        tracing::debug!("mcp_pool: skipping '{}' — command '{}' not found on host", server.name, command);
        return None;
    }

    // Container-only marker paths (built-in defaults target the Docker image).
    let container_only = args.iter().any(|a| a.starts_with("/home/claude-user"));
    if container_only {
        tracing::debug!("mcp_pool: skipping '{}' — definition targets container paths", server.name);
        return None;
    }

    Some(PooledServer {
        name: server.name.clone(),
        command: command.clone(),
        args: args.clone(),
        env: env.clone(),
    })
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
            cmd_server("container", "node", &["/home/claude-user/.npm-global/lib/x.js"]),
        );

        config
            .mcp_servers
            .insert("missing".into(), cmd_server("missing", "definitely-not-a-binary-xyz", &[]));

        let pooled = pooled_servers(&config);
        assert_eq!(pooled.len(), 1, "{pooled:?}");
        assert_eq!(pooled[0].name, "ok");
    }
}
