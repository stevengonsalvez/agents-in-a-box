// ABOUTME: MCP (Model Context Protocol) server configuration
// Manages MCP server definitions and installation

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Server name
    pub name: String,

    /// Server description
    pub description: String,

    /// Installation method
    pub installation: McpInstallation,

    /// Server definition for claude/gemini
    pub definition: McpServerDefinition,

    /// Required environment variables
    #[serde(default)]
    pub required_env: Vec<String>,

    /// Whether this server is enabled by default
    #[serde(default = "default_true")]
    pub enabled_by_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpInstallation {
    /// NPM package
    Npm {
        package: String,
        version: Option<String>,
    },

    /// Python package
    Python {
        package: String,
        version: Option<String>,
    },

    /// Git repository
    Git {
        url: String,
        branch: Option<String>,
        install_command: Option<String>,
    },

    /// Pre-installed (no action needed)
    PreInstalled,

    /// Custom installation script
    Custom { script: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpServerDefinition {
    /// Simple command-based server
    Command {
        command: String,
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },

    /// JSON-based configuration (for complex servers)
    Json { config: serde_json::Value },
}

fn default_true() -> bool {
    true
}

impl McpServerConfig {
    /// Create Serena MCP server config
    pub fn serena() -> Self {
        Self {
            name: "serena".to_string(),
            description: "AI coding agent that can work alongside you".to_string(),
            installation: McpInstallation::Npm {
                package: "@ambergristle/serena".to_string(),
                version: None,
            },
            definition: McpServerDefinition::Command {
                command: "node".to_string(),
                args: vec![
                    "/home/claude-user/.npm-global/lib/node_modules/@ambergristle/serena/out/index.js".to_string(),
                ],
                env: HashMap::new(),
            },
            required_env: vec![],
            enabled_by_default: true,
        }
    }

    /// Create Context7 MCP server config
    pub fn context7() -> Self {
        Self {
            name: "context7".to_string(),
            description: "Provides library documentation and code examples".to_string(),
            installation: McpInstallation::Npm {
                package: "context7".to_string(),
                version: None,
            },
            definition: McpServerDefinition::Command {
                command: "node".to_string(),
                args: vec![
                    "/home/claude-user/.npm-global/lib/node_modules/context7/index.js".to_string(),
                ],
                env: HashMap::new(),
            },
            required_env: vec![],
            enabled_by_default: true,
        }
    }

    /// Create Twilio MCP server config
    pub fn twilio() -> Self {
        Self {
            name: "twilio".to_string(),
            description: "Send SMS messages via Twilio".to_string(),
            installation: McpInstallation::Npm {
                package: "@twilio-labs/mcp-server-twilio".to_string(),
                version: None,
            },
            definition: McpServerDefinition::Json {
                config: serde_json::json!({
                    "command": "node",
                    "args": ["/home/claude-user/.npm-global/lib/node_modules/@twilio-labs/mcp-server-twilio/bin/run"],
                    "env": {
                        "TWILIO_AUTH_TOKEN": "${TWILIO_AUTH_TOKEN}",
                        "TWILIO_ACCOUNT_SID": "${TWILIO_ACCOUNT_SID}",
                        "TWILIO_FROM_PHONE": "${TWILIO_FROM_PHONE}"
                    }
                }),
            },
            required_env: vec![
                "TWILIO_AUTH_TOKEN".to_string(),
                "TWILIO_ACCOUNT_SID".to_string(),
                "TWILIO_FROM_PHONE".to_string(),
            ],
            enabled_by_default: false,
        }
    }

    /// Get default MCP server configurations
    pub fn defaults() -> HashMap<String, Self> {
        let mut servers = HashMap::new();

        let serena = Self::serena();
        servers.insert(serena.name.clone(), serena);

        let context7 = Self::context7();
        servers.insert(context7.name.clone(), context7);

        let twilio = Self::twilio();
        servers.insert(twilio.name.clone(), twilio);

        servers
    }

    /// Check if required environment variables are set
    pub fn check_env(&self) -> Result<(), Vec<String>> {
        let missing: Vec<String> = self
            .required_env
            .iter()
            .filter(|var| std::env::var(var).is_err())
            .cloned()
            .collect();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(missing)
        }
    }

    /// Generate installation script for this server
    pub fn installation_script(&self) -> String {
        match &self.installation {
            McpInstallation::Npm { package, version } => {
                if let Some(v) = version {
                    format!("npm install -g {}@{}", package, v)
                } else {
                    format!("npm install -g {}", package)
                }
            }
            McpInstallation::Python { package, version } => {
                if let Some(v) = version {
                    format!("pip install {}=={}", package, v)
                } else {
                    format!("pip install {}", package)
                }
            }
            McpInstallation::Git {
                url,
                branch,
                install_command,
            } => {
                let mut script = format!("git clone {}", url);
                if let Some(b) = branch {
                    script.push_str(&format!(" -b {}", b));
                }
                if let Some(cmd) = install_command {
                    script.push_str(&format!(" && {}", cmd));
                }
                script
            }
            McpInstallation::PreInstalled => "# Pre-installed".to_string(),
            McpInstallation::Custom { script } => script.clone(),
        }
    }

    /// Convert to MCP configuration format for Claude/Gemini
    pub fn to_mcp_config(&self) -> serde_json::Value {
        match &self.definition {
            McpServerDefinition::Command { command, args, env } => {
                let mut config = serde_json::json!({
                    "command": command,
                    "args": args,
                });

                if !env.is_empty() {
                    config["env"] = serde_json::json!(env);
                }

                config
            }
            McpServerDefinition::Json { config } => config.clone(),
        }
    }
}

/// Generate MCP servers configuration file content
pub fn generate_mcp_config_json(servers: &[McpServerConfig]) -> serde_json::Value {
    let mut mcp_servers = serde_json::Map::new();

    for server in servers {
        if server.enabled_by_default {
            mcp_servers.insert(server.name.clone(), server.to_mcp_config());
        }
    }

    serde_json::json!({
        "mcpServers": mcp_servers
    })
}

/// MCP initialization strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum McpInitStrategy {
    /// Initialize MCP servers inside each container
    PerContainer,

    /// Mount central MCP configuration from host
    CentralMount {
        /// Path to mount from (e.g., ~/.claude)
        host_path: String,
    },

    /// Hybrid: install servers in container but use central config
    Hybrid {
        /// Config path to mount
        config_path: String,
        /// Whether to merge with container config
        merge_configs: bool,
    },
}

impl Default for McpInitStrategy {
    fn default() -> Self {
        // Default to hybrid approach for flexibility
        Self::Hybrid {
            config_path: "~/.claude".to_string(),
            merge_configs: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serena_config() {
        let serena = McpServerConfig::serena();
        assert_eq!(serena.name, "serena");
        assert!(serena.enabled_by_default);
        assert!(serena.required_env.is_empty());
    }

    #[test]
    fn test_twilio_config() {
        let twilio = McpServerConfig::twilio();
        assert_eq!(twilio.name, "twilio");
        assert!(!twilio.enabled_by_default);
        assert_eq!(twilio.required_env.len(), 3);
    }

    #[test]
    fn test_installation_script() {
        let context7 = McpServerConfig::context7();
        let script = context7.installation_script();
        assert_eq!(script, "npm install -g context7");
    }

    #[test]
    fn test_mcp_config_generation() {
        let servers = vec![McpServerConfig::serena(), McpServerConfig::context7()];

        let config = generate_mcp_config_json(&servers);
        let mcp_servers = config["mcpServers"].as_object().unwrap();

        assert!(mcp_servers.contains_key("serena"));
        assert!(mcp_servers.contains_key("context7"));
    }
}
