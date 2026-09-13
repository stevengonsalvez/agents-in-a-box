// ABOUTME: Container template definitions for different development environments
// Provides pre-configured templates including claude-docker based setup

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerTemplate {
    /// Template name
    pub name: String,

    /// Template description
    pub description: String,

    /// Base configuration
    pub config: ContainerTemplateConfig,

    /// Required environment variables
    #[serde(default)]
    pub required_env: Vec<String>,

    /// Default MCP servers to include
    #[serde(default)]
    pub default_mcp_servers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerTemplateConfig {
    /// Docker image or Dockerfile path
    pub image_source: ImageSource,

    /// Working directory in container
    #[serde(default = "default_workdir")]
    pub working_dir: String,

    /// Command to run
    pub command: Option<Vec<String>>,

    /// Entrypoint override
    pub entrypoint: Option<Vec<String>>,

    /// Environment variables
    #[serde(default)]
    pub environment: HashMap<String, String>,

    /// User to run as (optional)
    pub user: Option<String>,

    /// Memory limit in MB
    pub memory_limit: Option<u64>,

    /// CPU limit (0.5 = half CPU)
    pub cpu_limit: Option<f64>,

    /// Additional packages to install
    #[serde(default)]
    pub system_packages: Vec<String>,

    /// NPM packages to install globally
    #[serde(default)]
    pub npm_packages: Vec<String>,

    /// Python packages to install
    #[serde(default)]
    pub python_packages: Vec<String>,

    /// Ports to expose
    #[serde(default)]
    pub ports: Vec<u16>,

    /// Additional volume mounts
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,

    /// Whether to mount SSH keys
    #[serde(default = "default_true")]
    pub mount_ssh: bool,

    /// Whether to mount git config
    #[serde(default = "default_true")]
    pub mount_git_config: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImageSource {
    /// Use a pre-built image from registry
    Image { name: String },

    /// Build from a Dockerfile
    Dockerfile {
        path: PathBuf,
        build_args: HashMap<String, String>,
    },

    /// Use claude-docker Dockerfile with modifications
    ClaudeDocker {
        /// Override base image
        base_image: Option<String>,
        /// Additional build args
        build_args: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    pub host_path: String,
    pub container_path: String,
    #[serde(default)]
    pub read_only: bool,
}

fn default_workdir() -> String {
    "/workspace".to_string()
}

fn default_true() -> bool {
    true
}

impl ContainerTemplate {
    /// Create the default Claude development template based on claude-docker
    pub fn claude_dev_default() -> Self {
        Self {
            name: "claude-dev".to_string(),
            description: "Claude development environment with MCP servers and AI tools".to_string(),
            config: ContainerTemplateConfig {
                image_source: ImageSource::ClaudeDocker {
                    base_image: None,
                    build_args: HashMap::new(),
                },
                working_dir: default_workdir(),
                command: None,
                entrypoint: Some(vec!["/app/scripts/startup.sh".to_string()]),
                environment: {
                    let mut env = HashMap::new();
                    env.insert("NODE_ENV".to_string(), "development".to_string());
                    env.insert("AGENTS_BOX_MODE".to_string(), "true".to_string());
                    env
                },
                user: Some("claude-user".to_string()),
                memory_limit: Some(4096), // 4GB
                cpu_limit: Some(2.0),     // 2 CPUs
                system_packages: vec![
                    "git".to_string(),
                    "curl".to_string(),
                    "build-essential".to_string(),
                    "python3".to_string(),
                    "python3-pip".to_string(),
                ],
                npm_packages: vec![
                    "@anthropic-ai/claude-code".to_string(),
                    "@google/gemini-cli".to_string(),
                ],
                python_packages: vec![],
                ports: vec![3000, 5173, 8080], // Common dev server ports
                volumes: vec![],
                mount_ssh: true,
                mount_git_config: true,
            },
            required_env: vec!["ANTHROPIC_API_KEY".to_string()],
            default_mcp_servers: vec!["serena".to_string(), "context7".to_string()],
        }
    }

    /// Create a Node.js development template
    pub fn node_default() -> Self {
        Self {
            name: "node".to_string(),
            description: "Node.js development environment".to_string(),
            config: ContainerTemplateConfig {
                image_source: ImageSource::Image {
                    name: "node:20-slim".to_string(),
                },
                working_dir: default_workdir(),
                command: Some(vec!["bash".to_string()]),
                entrypoint: None,
                environment: HashMap::new(),
                user: None,
                memory_limit: Some(2048),
                cpu_limit: Some(1.0),
                system_packages: vec!["git".to_string(), "build-essential".to_string()],
                npm_packages: vec![],
                python_packages: vec![],
                ports: vec![3000, 5173],
                volumes: vec![],
                mount_ssh: true,
                mount_git_config: true,
            },
            required_env: vec![],
            default_mcp_servers: vec![],
        }
    }

    /// Create a Python development template
    pub fn python_default() -> Self {
        Self {
            name: "python".to_string(),
            description: "Python development environment".to_string(),
            config: ContainerTemplateConfig {
                image_source: ImageSource::Image {
                    name: "python:3.11-slim".to_string(),
                },
                working_dir: default_workdir(),
                command: Some(vec!["bash".to_string()]),
                entrypoint: None,
                environment: HashMap::new(),
                user: None,
                memory_limit: Some(2048),
                cpu_limit: Some(1.0),
                system_packages: vec!["git".to_string(), "build-essential".to_string()],
                npm_packages: vec![],
                python_packages: vec![
                    "pip".to_string(),
                    "setuptools".to_string(),
                    "wheel".to_string(),
                ],
                ports: vec![8000, 5000],
                volumes: vec![],
                mount_ssh: true,
                mount_git_config: true,
            },
            required_env: vec![],
            default_mcp_servers: vec![],
        }
    }

    /// Create a Rust development template
    pub fn rust_default() -> Self {
        Self {
            name: "rust".to_string(),
            description: "Rust development environment".to_string(),
            config: ContainerTemplateConfig {
                image_source: ImageSource::Image {
                    name: "rust:1.75-slim".to_string(),
                },
                working_dir: default_workdir(),
                command: Some(vec!["bash".to_string()]),
                entrypoint: None,
                environment: HashMap::new(),
                user: None,
                memory_limit: Some(2048),
                cpu_limit: Some(1.0),
                system_packages: vec![
                    "git".to_string(),
                    "pkg-config".to_string(),
                    "libssl-dev".to_string(),
                ],
                npm_packages: vec![],
                python_packages: vec![],
                ports: vec![8080, 3000],
                volumes: vec![],
                mount_ssh: true,
                mount_git_config: true,
            },
            required_env: vec![],
            default_mcp_servers: vec![],
        }
    }

    /// Convert template to container config for session creation
    pub fn to_container_config(&self) -> crate::docker::ContainerConfig {
        use crate::docker::ContainerConfig;

        let mut config = match &self.config.image_source {
            ImageSource::Image { name } => ContainerConfig::new(name.clone()),
            ImageSource::Dockerfile { .. } => {
                // For now, we'll build the image separately and use the tag
                ContainerConfig::new("agents-box:custom".to_string())
            }
            ImageSource::ClaudeDocker { .. } => {
                ContainerConfig::new("agents-box:agents-dev".to_string())
            }
        };

        config.working_dir = self.config.working_dir.clone();

        if let Some(cmd) = &self.config.command {
            config = config.with_command(cmd.clone());
        }

        if let Some(entrypoint) = &self.config.entrypoint {
            config.entrypoint = Some(entrypoint.clone());
        }

        // Add environment variables
        for (k, v) in &self.config.environment {
            config = config.with_environment_var(k.clone(), v.clone());
        }

        // Set user
        if let Some(user) = &self.config.user {
            config.user = Some(user.clone());
        }

        // Set resource limits
        if let Some(memory) = self.config.memory_limit {
            config = config.with_memory_limit(memory * 1024 * 1024); // Convert MB to bytes
        }

        if let Some(cpu) = self.config.cpu_limit {
            config = config.with_cpu_limit(cpu);
        }

        // Add ports
        for port in &self.config.ports {
            config = config.with_port(*port, None); // Let Docker assign host port
        }

        // Add volumes
        for volume in &self.config.volumes {
            config = config.with_volume(
                PathBuf::from(&volume.host_path),
                volume.container_path.clone(),
                volume.read_only,
            );
        }

        config
    }
}

/// Build a Docker image from a template
pub async fn build_template_image(template: &ContainerTemplate, _tag: &str) -> anyhow::Result<()> {
    match &template.config.image_source {
        ImageSource::Image { .. } => {
            // Nothing to build, just use the image
            Ok(())
        }
        ImageSource::Dockerfile {
            path: _path,
            build_args: _build_args,
        } => {
            // TODO: Implement Dockerfile building
            todo!("Dockerfile building not yet implemented")
        }
        ImageSource::ClaudeDocker {
            base_image: _base_image,
            build_args: _build_args,
        } => {
            // TODO: Build using claude-docker as base
            todo!("Claude-docker building not yet implemented")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_dev_template() {
        let template = ContainerTemplate::claude_dev_default();
        assert_eq!(template.name, "claude-dev");
        assert!(matches!(
            template.config.image_source,
            ImageSource::ClaudeDocker { .. }
        ));
        assert_eq!(template.required_env, vec!["ANTHROPIC_API_KEY"]);
        assert_eq!(template.default_mcp_servers.len(), 2);
    }

    #[test]
    fn test_template_to_container_config() {
        let template = ContainerTemplate::node_default();
        let config = template.to_container_config();

        assert_eq!(config.image, "node:20-slim");
        assert_eq!(config.working_dir, "/workspace");
        assert_eq!(config.memory_limit, Some(2048 * 1024 * 1024));
    }
}
