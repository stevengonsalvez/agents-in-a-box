// ABOUTME: Docker container management using Bollard for creating and managing development containers

#![allow(dead_code)]

use super::{ContainerConfig, ContainerStatus, SessionContainer};
use anyhow::Result;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogOutput, LogsOptions,
    RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::{CreateImageOptions, ListImagesOptions};
use bollard::models::{
    ContainerSummary, HostConfig, HostConfigLogConfig, Mount, MountTypeEnum, PortBinding,
};
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum ContainerError {
    #[error("Docker connection error: {0}")]
    Connection(#[from] bollard::errors::Error),
    #[error("Container not found: {0}")]
    NotFound(String),
    #[error("Container already exists: {0}")]
    AlreadyExists(String),
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("Container operation failed: {0}")]
    OperationFailed(String),
}

/// Options for running a container
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub image: String,
    pub command: Vec<String>,
    pub env_vars: HashMap<String, String>,
    pub mounts: Vec<(PathBuf, PathBuf)>, // (host_path, container_path)
    pub working_dir: Option<String>,
    pub user: Option<String>,
    pub network: Option<String>,
    pub ports: Vec<(u16, u16)>, // (host_port, container_port)
    pub remove_on_exit: bool,
    pub interactive: bool,
    pub tty: bool,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<f64>,
    pub gpu_access: Option<String>,
    pub labels: HashMap<String, String>,
}

#[derive(Debug)]
pub struct ContainerManager {
    docker: Docker,
}

impl ContainerManager {
    pub async fn new() -> Result<Self, ContainerError> {
        let docker = Self::connect_to_docker().map_err(ContainerError::Connection)?;

        // Test the connection with configurable timeout (default: 60s)
        let timeout_secs = crate::config::AppConfig::load()
            .map(|c| c.docker.timeout)
            .unwrap_or(60);
        let ping_timeout = std::time::Duration::from_secs(timeout_secs);
        tokio::time::timeout(ping_timeout, docker.ping())
            .await
            .map_err(|_| {
                ContainerError::Connection(bollard::errors::Error::DockerResponseServerError {
                    status_code: 408,
                    message: "Docker ping timeout - daemon may be unresponsive".to_string(),
                })
            })?
            .map_err(ContainerError::Connection)?;

        info!("Successfully connected to Docker daemon");
        Ok(Self { docker })
    }

    pub fn new_sync() -> Result<Self, ContainerError> {
        let docker = Self::connect_to_docker().map_err(ContainerError::Connection)?;

        info!("Successfully connected to Docker daemon (sync)");
        Ok(Self { docker })
    }

    pub fn get_docker_client(&self) -> Docker {
        self.docker.clone()
    }

    pub fn connect_to_docker() -> Result<Docker, bollard::errors::Error> {
        // Try configuration file first
        if let Ok(config) = crate::config::AppConfig::load() {
            if let Some(docker_host) = &config.docker.host {
                info!("Using Docker host from config: {}", docker_host);
                std::env::set_var("DOCKER_HOST", docker_host);

                match Docker::connect_with_local_defaults() {
                    Ok(docker) => return Ok(docker),
                    Err(e) => {
                        warn!(
                            "Failed to connect to configured Docker host {}: {}",
                            docker_host, e
                        );
                        // Continue with other detection methods
                    }
                }
            }
        }

        // Try environment variable next
        if let Ok(docker_host) = std::env::var("DOCKER_HOST") {
            info!("Using DOCKER_HOST: {}", docker_host);
            return Docker::connect_with_local_defaults();
        }

        // Try common Docker socket locations based on OS
        let socket_paths = Self::get_docker_socket_paths();

        for socket_path in socket_paths {
            let exists = if socket_path.starts_with("npipe:") {
                // For Windows named pipes, we can't check existence easily
                // Just try to connect
                true
            } else {
                std::path::Path::new(&socket_path).exists()
            };

            if exists {
                info!("Found Docker socket at: {}", socket_path);

                // Set DOCKER_HOST environment variable for this process
                let docker_host = if socket_path.starts_with("npipe:") {
                    socket_path.clone()
                } else {
                    format!("unix://{}", socket_path)
                };

                std::env::set_var("DOCKER_HOST", docker_host);

                match Docker::connect_with_local_defaults() {
                    Ok(docker) => return Ok(docker),
                    Err(e) => {
                        warn!("Failed to connect to Docker socket {}: {}", socket_path, e);
                        continue;
                    }
                }
            }
        }

        // Fall back to default connection
        warn!("No Docker socket found, trying default connection");
        Docker::connect_with_local_defaults()
    }

    fn get_docker_socket_paths() -> Vec<String> {
        let mut paths = Vec::new();

        // Try to get Docker context information first
        if let Some(context_socket) = Self::get_docker_context_socket() {
            paths.push(context_socket);
        }

        // macOS specific paths
        if cfg!(target_os = "macos") {
            // Docker Desktop for Mac
            if let Some(home) = std::env::var("HOME").ok() {
                paths.push(format!("{}/.docker/run/docker.sock", home));
            }

            // Colima
            if let Some(home) = std::env::var("HOME").ok() {
                paths.push(format!("{}/.colima/default/docker.sock", home));
            }

            // Podman Desktop
            if let Some(home) = std::env::var("HOME").ok() {
                paths.push(format!(
                    "{}/.local/share/containers/podman/machine/podman.sock",
                    home
                ));
            }
        }

        // Linux specific paths
        if cfg!(target_os = "linux") {
            // Standard Docker socket
            paths.push("/var/run/docker.sock".to_string());

            // Rootless Docker
            if let Ok(xdg_runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
                paths.push(format!("{}/docker.sock", xdg_runtime_dir));
            }

            // Podman
            if let Ok(xdg_runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
                paths.push(format!("{}/podman/podman.sock", xdg_runtime_dir));
            }
        }

        // Windows specific paths
        if cfg!(target_os = "windows") {
            // Docker Desktop for Windows (named pipe)
            paths.push("npipe:////./pipe/docker_engine".to_string());

            // WSL2 integration
            paths.push("/var/run/docker.sock".to_string());
        }

        paths
    }

    fn get_docker_context_socket() -> Option<String> {
        // Try to get the current Docker context
        let output = std::process::Command::new("docker")
            .args([
                "context",
                "inspect",
                "--format",
                "{{.Endpoints.docker.Host}}",
            ])
            .output()
            .ok()?;

        if output.status.success() {
            let socket_url = String::from_utf8(output.stdout).ok()?;
            let socket_url = socket_url.trim();

            // Extract the socket path from the URL
            if let Some(path) = socket_url.strip_prefix("unix://") {
                debug!("Docker context socket: {}", path);
                return Some(path.to_string());
            } else if socket_url.starts_with("npipe:") {
                debug!("Docker context named pipe: {}", socket_url);
                return Some(socket_url.to_string());
            }
        }

        None
    }

    pub async fn create_session_container(
        &self,
        session_id: Uuid,
        config: ContainerConfig,
    ) -> Result<SessionContainer, ContainerError> {
        self.create_session_container_with_logs(session_id, config, None).await
    }

    pub async fn create_session_container_with_logs(
        &self,
        session_id: Uuid,
        config: ContainerConfig,
        log_sender: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<SessionContainer, ContainerError> {
        info!("Creating container for session {}", session_id);

        // Generate a container name
        let container_name = format!("agents-session-{}", session_id);

        // Check if container already exists
        if self.container_exists(&container_name).await? {
            return Err(ContainerError::AlreadyExists(container_name));
        }

        // Ensure image exists
        self.ensure_image_available(&config.image, log_sender).await?;

        // Create port bindings
        let mut port_bindings = HashMap::new();
        for port_mapping in &config.ports {
            let host_port =
                port_mapping.host_port.map(|p| p.to_string()).unwrap_or_else(|| "".to_string()); // Empty string for auto-assignment

            let container_port_key =
                format!("{}/{}", port_mapping.container_port, port_mapping.protocol);
            port_bindings.insert(
                container_port_key,
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some(host_port),
                }]),
            );
        }

        // Create volume mounts
        let mut mounts = Vec::new();
        for volume in &config.volumes {
            mounts.push(Mount {
                target: Some(volume.container_path.clone()),
                source: Some(volume.host_path.to_string_lossy().to_string()),
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(volume.read_only),
                consistency: Some("delegated".to_string()), // Better performance on macOS
                ..Default::default()
            });
        }

        // Create host config
        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            mounts: Some(mounts),
            memory: config.memory_limit.map(|m| m as i64),
            nano_cpus: config.cpu_limit.map(|c| (c * 1_000_000_000.0) as i64),
            auto_remove: Some(false), // We want to manage lifecycle manually
            log_config: Some(HostConfigLogConfig {
                typ: Some("json-file".to_string()),
                config: Some({
                    let mut log_config = HashMap::new();
                    log_config.insert("max-size".to_string(), "10m".to_string());
                    log_config.insert("max-file".to_string(), "3".to_string());
                    log_config
                }),
            }),
            ..Default::default()
        };

        // Prepare environment variables
        let env: Vec<String> =
            config.environment_vars.iter().map(|(k, v)| format!("{}={}", k, v)).collect();

        // Create container config
        let container_config = Config {
            image: Some(config.image.clone()),
            working_dir: Some(config.working_dir.clone()),
            env: Some(env),
            cmd: config.command.clone(),
            entrypoint: config.entrypoint.clone(),
            user: config.user.clone(),
            host_config: Some(host_config),
            labels: Some({
                let mut labels = HashMap::new();
                labels.insert("agents-session-id".to_string(), session_id.to_string());
                labels.insert("agents-managed".to_string(), "true".to_string());
                labels
            }),
            ..Default::default()
        };

        // Create the container
        let create_options = CreateContainerOptions {
            name: container_name.clone(),
            platform: None,
        };

        let create_response =
            self.docker.create_container(Some(create_options), container_config).await?;

        info!(
            "Created container {} with ID {}",
            container_name, create_response.id
        );

        let mut container = SessionContainer::new(session_id, config);
        container.container_id = Some(create_response.id.clone());
        container.status = ContainerStatus::Stopped;

        // Get the actual port mappings after creation
        container.host_ports = self.get_container_port_mappings(&create_response.id).await?;

        Ok(container)
    }

    pub async fn start_container(
        &self,
        container: &mut SessionContainer,
    ) -> Result<(), ContainerError> {
        let container_id = container
            .container_id
            .as_ref()
            .ok_or_else(|| ContainerError::InvalidConfig("No container ID".to_string()))?;

        info!("Starting container {}", container_id);

        container.status = ContainerStatus::Creating;

        self.docker
            .start_container(container_id, None::<StartContainerOptions<String>>)
            .await?;

        // Update port mappings (they might change on restart)
        container.host_ports = self.get_container_port_mappings(container_id).await?;
        container.status = ContainerStatus::Running;
        container.started_at = Some(chrono::Utc::now());

        info!("Successfully started container {}", container_id);
        Ok(())
    }

    pub async fn stop_container(
        &self,
        container: &mut SessionContainer,
    ) -> Result<(), ContainerError> {
        let container_id = container
            .container_id
            .as_ref()
            .ok_or_else(|| ContainerError::InvalidConfig("No container ID".to_string()))?;

        info!("Stopping container {}", container_id);

        let stop_options = StopContainerOptions { t: 10 }; // 10 second grace period

        match self.docker.stop_container(container_id, Some(stop_options)).await {
            Ok(_) => {
                container.status = ContainerStatus::Stopped;
                container.finished_at = Some(chrono::Utc::now());
                info!("Successfully stopped container {}", container_id);
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => {
                // Container was already stopped
                container.status = ContainerStatus::Stopped;
                debug!("Container {} was already stopped", container_id);
                Ok(())
            }
            Err(e) => Err(ContainerError::Connection(e)),
        }
    }

    pub async fn remove_container(
        &self,
        container: &mut SessionContainer,
    ) -> Result<(), ContainerError> {
        let container_id = container
            .container_id
            .as_ref()
            .ok_or_else(|| ContainerError::InvalidConfig("No container ID".to_string()))?
            .clone();

        info!("Removing container {}", container_id);

        // Stop the container first if it's running
        if container.is_running() {
            self.stop_container(container).await?;
        }

        let remove_options = RemoveContainerOptions {
            force: true,
            v: true, // Remove associated volumes
            ..Default::default()
        };

        match self.docker.remove_container(&container_id, Some(remove_options)).await {
            Ok(_) => {
                container.status = ContainerStatus::NotFound;
                container.container_id = None;
                info!("Successfully removed container {}", container_id);
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                // Container was already removed
                container.status = ContainerStatus::NotFound;
                container.container_id = None;
                debug!("Container {} was already removed", container_id);
                Ok(())
            }
            Err(e) => Err(ContainerError::Connection(e)),
        }
    }

    /// Remove a container by its ID directly
    ///
    /// This is useful for cleanup operations where we only have the container ID
    /// and don't need to update a SessionContainer struct.
    pub async fn remove_container_by_id(&self, container_id: &str) -> Result<(), ContainerError> {
        info!("Removing container by ID: {}", container_id);

        // Try to stop the container first (ignore errors if it's already stopped)
        let stop_options = StopContainerOptions { t: 10 };
        if let Err(e) = self.docker.stop_container(container_id, Some(stop_options)).await {
            debug!(
                "Failed to stop container {} (may already be stopped): {}",
                container_id, e
            );
        }

        let remove_options = RemoveContainerOptions {
            force: true,
            v: true, // Remove associated volumes
            ..Default::default()
        };

        match self.docker.remove_container(container_id, Some(remove_options)).await {
            Ok(_) => {
                info!("Successfully removed container {}", container_id);
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                // Container was already removed
                debug!("Container {} was already removed", container_id);
                Ok(())
            }
            Err(e) => Err(ContainerError::Connection(e)),
        }
    }

    pub async fn get_container_status(
        &self,
        container_id: &str,
    ) -> Result<ContainerStatus, ContainerError> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: true,
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("id".to_string(), vec![container_id.to_string()]);
                    filters
                },
                ..Default::default()
            }))
            .await?;

        if let Some(container) = containers.first() {
            let status = container.state.as_deref().unwrap_or("unknown");
            match status {
                "running" => Ok(ContainerStatus::Running),
                "paused" => Ok(ContainerStatus::Paused),
                "exited" | "dead" => Ok(ContainerStatus::Stopped),
                "created" => Ok(ContainerStatus::Creating),
                _ => Ok(ContainerStatus::Error(format!(
                    "Unknown status: {}",
                    status
                ))),
            }
        } else {
            Ok(ContainerStatus::NotFound)
        }
    }

    pub async fn get_container_logs(
        &self,
        container_id: &str,
        lines: Option<i64>,
    ) -> Result<Vec<String>, ContainerError> {
        let options = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            tail: lines.map(|n| n.to_string()).unwrap_or_else(|| "100".to_string()),
            ..Default::default()
        };

        let mut stream = self.docker.logs(container_id, Some(options));
        let mut logs = Vec::new();

        use futures_util::stream::StreamExt;
        while let Some(log_result) = stream.next().await {
            match log_result {
                Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                    if let Ok(log_line) = String::from_utf8(message.to_vec()) {
                        logs.push(log_line.trim_end().to_string());
                    }
                }
                Ok(_) => {} // Ignore other log types
                Err(e) => {
                    warn!("Error reading container logs: {}", e);
                    break;
                }
            }
        }

        Ok(logs)
    }

    pub async fn list_agents_containers(&self) -> Result<Vec<ContainerSummary>, ContainerError> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: true,
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("label".to_string(), vec!["agents-managed=true".to_string()]);
                    filters
                },
                ..Default::default()
            }))
            .await?;

        Ok(containers)
    }

    async fn container_exists(&self, name: &str) -> Result<bool, ContainerError> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: true,
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("name".to_string(), vec![name.to_string()]);
                    filters
                },
                ..Default::default()
            }))
            .await?;

        Ok(!containers.is_empty())
    }

    async fn ensure_image_available(
        &self,
        image: &str,
        log_sender: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<(), ContainerError> {
        // Check if image exists locally
        let images = self
            .docker
            .list_images(Some(ListImagesOptions::<String> {
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("reference".to_string(), vec![image.to_string()]);
                    filters
                },
                ..Default::default()
            }))
            .await?;

        if !images.is_empty() {
            debug!("Image {} already exists locally", image);
            return Ok(());
        }

        // Check if this is a local build image (agents-box:*)
        if image.starts_with("agents-box:") {
            info!("Building local image {}", image);

            // Extract template name from image tag
            let template_name = image.strip_prefix("agents-box:").unwrap_or("agents-dev");

            // Get the appropriate template
            let app_config = crate::config::AppConfig::load().map_err(|e| {
                ContainerError::OperationFailed(format!("Failed to load config: {}", e))
            })?;

            let template = app_config.get_container_template(template_name).ok_or_else(|| {
                ContainerError::OperationFailed(format!("Template '{}' not found", template_name))
            })?;

            // Build the image using ImageBuilder
            let builder = super::ImageBuilder::new().await.map_err(|e| {
                ContainerError::OperationFailed(format!("Failed to create image builder: {}", e))
            })?;

            builder
                .build_template_with_logs(template, image, log_sender)
                .await
                .map_err(|e| {
                    ContainerError::OperationFailed(format!("Failed to build image: {}", e))
                })?;

            info!("Successfully built local image {}", image);
            return Ok(());
        }

        info!("Pulling image {}", image);

        let create_image_options = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };

        use futures_util::stream::StreamExt;
        let mut stream = self.docker.create_image(Some(create_image_options), None, None);

        while let Some(result) = stream.next().await {
            match result {
                Ok(_) => {} // Progress update
                Err(e) => {
                    error!("Failed to pull image {}: {}", image, e);
                    return Err(ContainerError::OperationFailed(format!(
                        "Failed to pull image: {}",
                        e
                    )));
                }
            }
        }

        info!("Successfully pulled image {}", image);
        Ok(())
    }

    async fn get_container_port_mappings(
        &self,
        container_id: &str,
    ) -> Result<HashMap<u16, u16>, ContainerError> {
        let container = self.docker.inspect_container(container_id, None).await?;
        let mut port_mappings = HashMap::new();

        if let Some(network_settings) = container.network_settings {
            if let Some(ports) = network_settings.ports {
                for (container_port_key, host_ports) in ports {
                    if let Some(host_ports) = host_ports {
                        for host_port in host_ports {
                            if let (Ok(container_port), Some(host_port_str)) = (
                                container_port_key.split('/').next().unwrap_or("").parse::<u16>(),
                                &host_port.host_port,
                            ) {
                                if let Ok(host_port) = host_port_str.parse::<u16>() {
                                    port_mappings.insert(container_port, host_port);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(port_mappings)
    }

    /// Run a container with the given options
    pub async fn run_container(
        &self,
        name: &str,
        options: &RunOptions,
    ) -> Result<String, ContainerError> {
        info!("Running container: {}", name);

        // Check if container already exists
        if self.container_exists(name).await? {
            return Err(ContainerError::AlreadyExists(name.to_string()));
        }

        // Create volume mounts
        let mut mounts = Vec::new();
        for (host_path, container_path) in &options.mounts {
            mounts.push(Mount {
                target: Some(container_path.to_string_lossy().to_string()),
                source: Some(host_path.to_string_lossy().to_string()),
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(false),
                consistency: Some("delegated".to_string()),
                ..Default::default()
            });
        }

        // Create port bindings
        let mut port_bindings = HashMap::new();
        for (host_port, container_port) in &options.ports {
            let container_port_key = format!("{}/tcp", container_port);
            port_bindings.insert(
                container_port_key,
                Some(vec![PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some(host_port.to_string()),
                }]),
            );
        }

        // Parse memory limit
        let memory_limit = options.memory_limit.as_ref().and_then(|limit| {
            // Parse formats like "4g", "2048m", "1024"
            let limit = limit.to_lowercase();
            if limit.ends_with("g") {
                limit[..limit.len() - 1]
                    .parse::<f64>()
                    .ok()
                    .map(|g| (g * 1024.0 * 1024.0 * 1024.0) as i64)
            } else if limit.ends_with("m") {
                limit[..limit.len() - 1]
                    .parse::<f64>()
                    .ok()
                    .map(|m| (m * 1024.0 * 1024.0) as i64)
            } else {
                limit.parse::<i64>().ok()
            }
        });

        // Create host config
        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            mounts: Some(mounts),
            memory: memory_limit,
            nano_cpus: options.cpu_limit.map(|c| (c * 1_000_000_000.0) as i64),
            auto_remove: Some(options.remove_on_exit),
            ..Default::default()
        };

        // Prepare environment variables
        let env: Vec<String> =
            options.env_vars.iter().map(|(k, v)| format!("{}={}", k, v)).collect();

        // Create container config
        let container_config = Config {
            image: Some(options.image.clone()),
            working_dir: options.working_dir.clone(),
            env: Some(env),
            cmd: if options.command.is_empty() {
                None
            } else {
                Some(options.command.clone())
            },
            user: options.user.clone(),
            attach_stdin: Some(options.interactive),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(options.tty),
            open_stdin: Some(options.interactive),
            host_config: Some(host_config),
            labels: Some({
                let mut labels = HashMap::new();
                labels.insert("agents-managed".to_string(), "true".to_string());
                labels.insert("agents-dev".to_string(), "true".to_string());
                // Add any custom labels
                for (key, value) in &options.labels {
                    labels.insert(key.clone(), value.clone());
                }
                labels
            }),
            ..Default::default()
        };

        // Create the container
        let create_options = CreateContainerOptions {
            name: name.to_string(),
            platform: None,
        };

        let create_response =
            self.docker.create_container(Some(create_options), container_config).await?;

        info!("Created container {} with ID {}", name, create_response.id);

        // Start the container
        self.docker
            .start_container(&create_response.id, None::<StartContainerOptions<String>>)
            .await?;

        info!("Started container: {}", create_response.id);
        Ok(create_response.id)
    }

    /// Execute a command in a running container with TTY support
    pub async fn exec_interactive(
        &self,
        container_id: &str,
        command: Vec<String>,
    ) -> Result<tokio::process::Child, ContainerError> {
        use std::process::Stdio;
        use tokio::process::Command;

        info!(
            "Executing interactive command in container {}: {:?}",
            container_id, command
        );

        // Use docker CLI for better TTY support than Bollard
        let mut cmd = Command::new("docker");
        cmd.arg("exec").arg("-it").arg(container_id);

        for arg in command {
            cmd.arg(arg);
        }

        cmd.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let child = cmd.spawn().map_err(|e| {
            ContainerError::OperationFailed(format!("Failed to spawn docker exec: {}", e))
        })?;

        Ok(child)
    }

    /// Execute a command interactively with proper terminal handling (blocks until completion)
    pub async fn exec_interactive_blocking(
        &self,
        container_id: &str,
        command: Vec<String>,
    ) -> Result<std::process::ExitStatus, ContainerError> {
        use crossterm::{
            execute,
            terminal::{
                EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
            },
        };
        use std::io;
        use std::process::{Command, Stdio};

        info!(
            "Executing blocking interactive command in container {}: {:?}",
            container_id, command
        );

        // Exit TUI mode temporarily
        disable_raw_mode().map_err(|e| {
            ContainerError::OperationFailed(format!("Failed to disable raw mode: {}", e))
        })?;
        execute!(io::stdout(), LeaveAlternateScreen).map_err(|e| {
            ContainerError::OperationFailed(format!("Failed to leave alternate screen: {}", e))
        })?;

        // Execute docker command in foreground
        let mut cmd = Command::new("docker");
        cmd.arg("exec").arg("-it").arg(container_id);

        for arg in command {
            cmd.arg(arg);
        }

        cmd.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let result = cmd.status();

        // Restore TUI mode
        enable_raw_mode().map_err(|e| {
            ContainerError::OperationFailed(format!("Failed to re-enable raw mode: {}", e))
        })?;
        execute!(io::stdout(), EnterAlternateScreen).map_err(|e| {
            ContainerError::OperationFailed(format!("Failed to re-enter alternate screen: {}", e))
        })?;

        match result {
            Ok(status) => Ok(status),
            Err(e) => Err(ContainerError::OperationFailed(format!(
                "Failed to execute docker command: {}",
                e
            ))),
        }
    }

    /// Execute a command in a running container (non-interactive)
    pub async fn exec_command(
        &self,
        container_id: &str,
        command: Vec<String>,
    ) -> Result<Vec<u8>, ContainerError> {
        info!(
            "Executing command in container {}: {:?}",
            container_id, command
        );

        let exec_options = CreateExecOptions {
            cmd: Some(command),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let exec = self.docker.create_exec(container_id, exec_options).await?;

        let mut result_output = Vec::new();
        if let StartExecResults::Attached { mut output, .. } =
            self.docker.start_exec(&exec.id, None).await?
        {
            while let Some(Ok(msg)) = output.next().await {
                match msg {
                    bollard::container::LogOutput::StdOut { message } => {
                        result_output.extend_from_slice(&message);
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        result_output.extend_from_slice(&message);
                    }
                    _ => {}
                }
            }
        }

        Ok(result_output)
    }

    /// Get the latest log file from the container
    pub async fn get_latest_log_file(
        &self,
        container_id: &str,
    ) -> Result<Option<String>, ContainerError> {
        let command = vec![
            "bash".to_string(),
            "-c".to_string(),
            "ls -t /workspace/.claude-box/logs/claude-*.log 2>/dev/null | head -n1".to_string(),
        ];

        let output = self.exec_command(container_id, command).await?;
        let log_path = String::from_utf8(output)
            .map_err(|e| {
                ContainerError::OperationFailed(format!("Failed to parse log path: {}", e))
            })?
            .trim()
            .to_string();

        if log_path.is_empty() {
            Ok(None)
        } else {
            Ok(Some(log_path))
        }
    }

    /// Tail logs from the container
    pub async fn tail_logs(
        &self,
        container_id: &str,
        lines: usize,
    ) -> Result<String, ContainerError> {
        // First get the latest log file
        let log_file = self.get_latest_log_file(container_id).await?;

        if let Some(log_path) = log_file {
            let command = vec!["tail".to_string(), format!("-n{}", lines), log_path];

            let output = self.exec_command(container_id, command).await?;
            String::from_utf8(output).map_err(|e| {
                ContainerError::OperationFailed(format!("Failed to parse logs: {}", e))
            })
        } else {
            Ok("No Claude logs found yet.".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Note: These tests require Docker to be running
    // They are integration tests and should be run with `cargo test --ignored`

    #[tokio::test]
    #[ignore]
    async fn test_container_manager_creation() {
        let manager = ContainerManager::new().await;
        assert!(manager.is_ok(), "Should be able to connect to Docker");
    }

    #[tokio::test]
    #[ignore]
    async fn test_container_lifecycle() {
        let manager = ContainerManager::new().await.unwrap();
        let session_id = Uuid::new_v4();
        let temp_dir = TempDir::new().unwrap();

        let config = ContainerConfig::new("alpine:latest".to_string())
            .with_command(vec!["sleep".to_string(), "30".to_string()])
            .with_volume(
                temp_dir.path().to_path_buf(),
                "/workspace".to_string(),
                false,
            );

        // Create container
        let mut container = manager.create_session_container(session_id, config).await.unwrap();
        assert!(container.container_id.is_some());
        assert_eq!(container.status, ContainerStatus::Stopped);

        // Start container
        manager.start_container(&mut container).await.unwrap();
        assert_eq!(container.status, ContainerStatus::Running);

        // Stop container
        manager.stop_container(&mut container).await.unwrap();
        assert_eq!(container.status, ContainerStatus::Stopped);

        // Remove container
        manager.remove_container(&mut container).await.unwrap();
        assert_eq!(container.status, ContainerStatus::NotFound);
        assert!(container.container_id.is_none());
    }
}
