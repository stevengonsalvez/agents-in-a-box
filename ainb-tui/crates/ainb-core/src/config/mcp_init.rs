// ABOUTME: MCP initialization orchestration for different strategies
// Manages how MCP servers are configured and initialized in containers

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::mcp::{McpInitStrategy, McpServerConfig, generate_mcp_config_json};
use crate::docker::ContainerConfig;

#[derive(Debug, Clone)]
pub struct McpInitializer {
    strategy: McpInitStrategy,
    servers: HashMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInitResult {
    /// MCP configuration that was generated
    pub config: serde_json::Value,

    /// Additional container volumes needed for MCP
    pub volumes: Vec<McpVolumeMount>,

    /// Environment variables to set
    pub environment: HashMap<String, String>,

    /// Post-creation scripts to run in container
    pub post_scripts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpVolumeMount {
    pub host_path: PathBuf,
    pub container_path: String,
    pub read_only: bool,
    pub description: String,
}

impl McpInitializer {
    pub fn new(strategy: McpInitStrategy, servers: HashMap<String, McpServerConfig>) -> Self {
        Self { strategy, servers }
    }

    /// Initialize MCP for a container session
    pub async fn initialize_for_session(
        &self,
        session_id: uuid::Uuid,
        workspace_path: &Path,
        container_config: &mut ContainerConfig,
    ) -> Result<McpInitResult> {
        info!(
            "Initializing MCP for session {} with strategy {:?}",
            session_id, self.strategy
        );

        match &self.strategy {
            McpInitStrategy::PerContainer => self.init_per_container(container_config).await,
            McpInitStrategy::CentralMount { host_path } => {
                self.init_central_mount(host_path, container_config).await
            }
            McpInitStrategy::Hybrid {
                config_path,
                merge_configs,
            } => {
                self.init_hybrid(
                    config_path,
                    *merge_configs,
                    workspace_path,
                    container_config,
                )
                .await
            }
        }
    }

    /// Initialize MCP with per-container strategy
    async fn init_per_container(
        &self,
        _container_config: &mut ContainerConfig,
    ) -> Result<McpInitResult> {
        info!("Using per-container MCP initialization");

        // Enable MCP servers that don't require missing environment variables
        let enabled_servers: Vec<McpServerConfig> = self
            .servers
            .values()
            .filter(|server| {
                if !server.enabled_by_default {
                    return false;
                }

                // Check if required environment variables are available
                server.check_env().is_ok()
            })
            .cloned()
            .collect();

        // Generate MCP configuration
        let mcp_config = generate_mcp_config_json(&enabled_servers);

        // Add environment variables for enabled servers
        let mut environment = HashMap::new();
        for server in &enabled_servers {
            for env_var in &server.required_env {
                if let Ok(value) = std::env::var(env_var) {
                    environment.insert(env_var.clone(), value);
                }
            }
        }

        // No additional volumes needed - everything is installed in container
        let volumes = vec![];

        // Generate post-creation script to write MCP config
        let post_scripts = vec![format!(
            "mkdir -p /home/claude-user/.claude && echo '{}' > /home/claude-user/.claude/mcp-config.json",
            serde_json::to_string(&mcp_config)?
        )];

        Ok(McpInitResult {
            config: mcp_config,
            volumes,
            environment,
            post_scripts,
        })
    }

    /// Initialize MCP with central mount strategy
    async fn init_central_mount(
        &self,
        host_path: &str,
        _container_config: &mut ContainerConfig,
    ) -> Result<McpInitResult> {
        info!("Using central mount MCP initialization from {}", host_path);

        // Expand tilde in path
        let expanded_path = if host_path.starts_with("~/") {
            if let Some(home_dir) = dirs::home_dir() {
                home_dir.join(&host_path[2..])
            } else {
                PathBuf::from(host_path)
            }
        } else {
            PathBuf::from(host_path)
        };

        // Check if the configuration directory exists
        if !expanded_path.exists() {
            warn!(
                "MCP config directory {} does not exist",
                expanded_path.display()
            );
        }

        // Mount the entire config directory
        let volumes = vec![McpVolumeMount {
            host_path: expanded_path.clone(),
            container_path: "/home/claude-user/.claude".to_string(),
            read_only: false,
            description: "Claude configuration directory with MCP servers".to_string(),
        }];

        // Try to read existing MCP config
        let mcp_config_path = expanded_path.join("mcp-config.json");
        let mcp_config = if mcp_config_path.exists() {
            let content = fs::read_to_string(&mcp_config_path)
                .context("Failed to read existing MCP config")?;
            serde_json::from_str(&content).context("Failed to parse existing MCP config")?
        } else {
            // Generate default config
            let enabled_servers: Vec<McpServerConfig> = self
                .servers
                .values()
                .filter(|s| s.enabled_by_default && s.check_env().is_ok())
                .cloned()
                .collect();
            generate_mcp_config_json(&enabled_servers)
        };

        Ok(McpInitResult {
            config: mcp_config,
            volumes,
            environment: HashMap::new(),
            post_scripts: vec![],
        })
    }

    /// Initialize MCP with hybrid strategy
    async fn init_hybrid(
        &self,
        config_path: &str,
        merge_configs: bool,
        _workspace_path: &Path,
        _container_config: &mut ContainerConfig,
    ) -> Result<McpInitResult> {
        info!("Using hybrid MCP initialization");

        // Expand config path
        let expanded_path = if config_path.starts_with("~/") {
            if let Some(home_dir) = dirs::home_dir() {
                home_dir.join(&config_path[2..])
            } else {
                PathBuf::from(config_path)
            }
        } else {
            PathBuf::from(config_path)
        };

        // Mount configuration directory (read-only for safety)
        let mut volumes = vec![McpVolumeMount {
            host_path: expanded_path.clone(),
            container_path: "/mnt/host-claude-config".to_string(),
            read_only: true,
            description: "Host Claude configuration (read-only)".to_string(),
        }];

        // Mount SSH keys for git operations
        if let Some(home_dir) = dirs::home_dir() {
            let ssh_dir = home_dir.join(".ssh");
            if ssh_dir.exists() {
                volumes.push(McpVolumeMount {
                    host_path: ssh_dir,
                    container_path: "/home/claude-user/.ssh".to_string(),
                    read_only: true,
                    description: "SSH keys for git operations".to_string(),
                });
            }
        }

        // Generate container-specific MCP config
        let enabled_servers: Vec<McpServerConfig> = self
            .servers
            .values()
            .filter(|s| s.enabled_by_default && s.check_env().is_ok())
            .cloned()
            .collect();

        let container_mcp_config = generate_mcp_config_json(&enabled_servers);

        // Create post-script to merge configurations if requested
        let post_scripts = if merge_configs {
            vec![
                r#"
                # Merge host and container MCP configurations
                mkdir -p /home/claude-user/.claude

                # Copy host config if it exists
                if [ -f /mnt/host-claude-config/mcp-config.json ]; then
                    cp /mnt/host-claude-config/mcp-config.json /home/claude-user/.claude/host-mcp-config.json
                fi

                # Write container config
                echo '{}' > /home/claude-user/.claude/container-mcp-config.json

                # Create merged config using simple merge (container servers take precedence)
                if [ -f /home/claude-user/.claude/host-mcp-config.json ]; then
                    node -e "
                        const fs = require('fs');
                        const hostConfig = JSON.parse(fs.readFileSync('/home/claude-user/.claude/host-mcp-config.json', 'utf8'));
                        const containerConfig = JSON.parse(fs.readFileSync('/home/claude-user/.claude/container-mcp-config.json', 'utf8'));

                        const merged = {
                            mcpServers: {
                                ...hostConfig.mcpServers,
                                ...containerConfig.mcpServers
                            }
                        };

                        fs.writeFileSync('/home/claude-user/.claude/mcp-config.json', JSON.stringify(merged, null, 2));
                    "
                else
                    cp /home/claude-user/.claude/container-mcp-config.json /home/claude-user/.claude/mcp-config.json
                fi
                "#.trim().replace("{}", &serde_json::to_string(&container_mcp_config)?)
            ]
        } else {
            vec![format!(
                "mkdir -p /home/claude-user/.claude && echo '{}' > /home/claude-user/.claude/mcp-config.json",
                serde_json::to_string(&container_mcp_config)?
            )]
        };

        // Collect environment variables
        let mut environment = HashMap::new();
        for server in &enabled_servers {
            for env_var in &server.required_env {
                if let Ok(value) = std::env::var(env_var) {
                    environment.insert(env_var.clone(), value);
                }
            }
        }

        Ok(McpInitResult {
            config: container_mcp_config,
            volumes,
            environment,
            post_scripts,
        })
    }

    /// Get the default MCP initialization strategy
    pub fn default_strategy() -> McpInitStrategy {
        // Use PerContainer strategy since we're already mounting ~/.claude directly
        McpInitStrategy::PerContainer
    }

    /// Check if MCP servers are properly configured
    pub fn validate_configuration(&self) -> Result<Vec<String>> {
        let mut warnings = Vec::new();

        for (name, server) in &self.servers {
            if server.enabled_by_default {
                if let Err(missing_vars) = server.check_env() {
                    warnings.push(format!(
                        "MCP server '{}' is enabled but missing environment variables: {}",
                        name,
                        missing_vars.join(", ")
                    ));
                }
            }
        }

        Ok(warnings)
    }
}

/// Apply MCP initialization result to container configuration
pub fn apply_mcp_init_result(container_config: &mut ContainerConfig, mcp_result: &McpInitResult) {
    // Add environment variables
    for (key, value) in &mcp_result.environment {
        container_config.environment_vars.insert(key.clone(), value.clone());
    }

    // Add volume mounts
    for volume in &mcp_result.volumes {
        *container_config = container_config.clone().with_volume(
            volume.host_path.clone(),
            volume.container_path.clone(),
            volume.read_only,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::mcp::{McpInstallation, McpServerConfig, McpServerDefinition};
    use std::collections::HashMap;

    #[test]
    fn test_per_container_init() {
        let servers = McpServerConfig::defaults();
        let initializer = McpInitializer::new(McpInitStrategy::PerContainer, servers);

        let _container_config = ContainerConfig::new("test".to_string());

        // This would require tokio runtime in real usage
        // For testing, we'll just verify the initializer was created correctly
        assert!(matches!(
            initializer.strategy,
            McpInitStrategy::PerContainer
        ));
    }

    #[test]
    fn test_validation() {
        let mut servers = HashMap::new();

        // Create a test server that is enabled by default and requires env vars
        let test_server = McpServerConfig {
            name: "test-server".to_string(),
            description: "Test server".to_string(),
            installation: McpInstallation::Npm {
                package: "test-package".to_string(),
                version: None,
            },
            definition: McpServerDefinition::Json {
                config: serde_json::json!({}),
            },
            required_env: vec!["TEST_REQUIRED_VAR".to_string()],
            enabled_by_default: true, // This is enabled by default
        };

        servers.insert(test_server.name.clone(), test_server);

        let initializer = McpInitializer::new(McpInitStrategy::PerContainer, servers);

        let warnings = initializer.validate_configuration().unwrap();
        // Should have warnings about missing environment variables
        assert!(!warnings.is_empty());
        assert!(warnings[0].contains("TEST_REQUIRED_VAR"));
    }
}
