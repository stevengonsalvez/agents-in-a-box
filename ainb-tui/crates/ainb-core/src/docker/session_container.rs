// ABOUTME: Session container data structures and configuration

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerStatus {
    Creating,
    Running,
    Paused,
    Stopped,
    Error(String),
    NotFound,
}

impl ContainerStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, ContainerStatus::Running)
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self, ContainerStatus::Stopped | ContainerStatus::NotFound)
    }

    pub fn indicator(&self) -> &'static str {
        match self {
            ContainerStatus::Creating => "⏳",
            ContainerStatus::Running => "🟢",
            ContainerStatus::Paused => "⏸️",
            ContainerStatus::Stopped => "⏹️",
            ContainerStatus::Error(_) => "🔴",
            ContainerStatus::NotFound => "❓",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    pub image: String,
    pub working_dir: String,
    pub environment_vars: HashMap<String, String>,
    pub volumes: Vec<VolumeMount>,
    pub ports: Vec<PortMapping>,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub user: Option<String>,
    pub memory_limit: Option<u64>, // bytes
    pub cpu_limit: Option<f64>,    // CPU shares (1.0 = 1 CPU)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    pub host_path: PathBuf,
    pub container_path: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub host_port: Option<u16>, // None for auto-assignment
    pub container_port: u16,
    pub protocol: String, // tcp, udp
}

#[derive(Debug, Clone)]
pub struct SessionContainer {
    pub session_id: Uuid,
    pub container_id: Option<String>,
    pub config: ContainerConfig,
    pub status: ContainerStatus,
    pub host_ports: HashMap<u16, u16>, // container_port -> host_port
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            image: "ubuntu:22.04".to_string(),
            working_dir: "/workspace".to_string(),
            environment_vars: HashMap::new(),
            volumes: Vec::new(),
            ports: Vec::new(),
            command: None,
            entrypoint: None,
            user: None,
            memory_limit: Some(2 * 1024 * 1024 * 1024), // 2GB default
            cpu_limit: Some(2.0),                       // 2 CPUs default
        }
    }
}

impl ContainerConfig {
    pub fn new(image: String) -> Self {
        Self {
            image,
            ..Default::default()
        }
    }

    pub fn with_working_dir(mut self, working_dir: String) -> Self {
        self.working_dir = working_dir;
        self
    }

    pub fn with_environment_var(mut self, key: String, value: String) -> Self {
        self.environment_vars.insert(key, value);
        self
    }

    pub fn with_volume(
        mut self,
        host_path: PathBuf,
        container_path: String,
        read_only: bool,
    ) -> Self {
        self.volumes.push(VolumeMount {
            host_path,
            container_path,
            read_only,
        });
        self
    }

    pub fn with_port(mut self, container_port: u16, host_port: Option<u16>) -> Self {
        self.ports.push(PortMapping {
            host_port,
            container_port,
            protocol: "tcp".to_string(),
        });
        self
    }

    pub fn with_command(mut self, command: Vec<String>) -> Self {
        self.command = Some(command);
        self
    }

    pub fn with_memory_limit(mut self, bytes: u64) -> Self {
        self.memory_limit = Some(bytes);
        self
    }

    pub fn with_cpu_limit(mut self, cpus: f64) -> Self {
        self.cpu_limit = Some(cpus);
        self
    }

    /// Create a Claude Code development environment configuration
    pub fn claude_dev_config(worktree_path: PathBuf) -> Self {
        let mut env_vars = HashMap::new();
        env_vars.insert("TERM".to_string(), "xterm-256color".to_string());
        env_vars.insert("SHELL".to_string(), "/bin/bash".to_string());
        env_vars.insert("USER".to_string(), "developer".to_string());
        env_vars.insert("HOME".to_string(), "/home/developer".to_string());

        Self {
            image: "ghcr.io/anthropics/claude-code:latest".to_string(),
            working_dir: "/workspace".to_string(),
            environment_vars: env_vars,
            volumes: vec![VolumeMount {
                host_path: worktree_path,
                container_path: "/workspace".to_string(),
                read_only: false,
            }],
            ports: vec![
                // Common development ports
                PortMapping {
                    host_port: None, // Auto-assign
                    container_port: 3000,
                    protocol: "tcp".to_string(),
                },
                PortMapping {
                    host_port: None, // Auto-assign
                    container_port: 8080,
                    protocol: "tcp".to_string(),
                },
            ],
            command: Some(vec!["/bin/bash".to_string()]),
            entrypoint: None,
            user: Some("developer".to_string()),
            memory_limit: Some(4 * 1024 * 1024 * 1024), // 4GB for development
            cpu_limit: Some(4.0),                       // 4 CPUs for development
        }
    }
}

impl SessionContainer {
    pub fn new(session_id: Uuid, config: ContainerConfig) -> Self {
        Self {
            session_id,
            container_id: None,
            config,
            status: ContainerStatus::NotFound,
            host_ports: HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            finished_at: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.status.is_running()
    }

    pub fn is_stopped(&self) -> bool {
        self.status.is_stopped()
    }

    pub fn get_host_port(&self, container_port: u16) -> Option<u16> {
        self.host_ports.get(&container_port).copied()
    }

    pub fn get_workspace_url(&self, port: u16) -> Option<String> {
        self.get_host_port(port)
            .map(|host_port| format!("http://localhost:{}", host_port))
    }
}
