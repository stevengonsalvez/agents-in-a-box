// ABOUTME: Docker image builder for container templates
// Handles building custom images from Dockerfiles and claude-docker base

#![allow(dead_code)]

use anyhow::{Context, Result};
use bollard::Docker;
use bollard::image::{BuildImageOptions, CreateImageOptions};
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tar::{Builder, Header};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::config::{ContainerTemplate, container::ImageSource};

pub struct ImageBuilder {
    docker: Docker,
}

#[derive(Debug)]
pub struct BuildContext {
    pub dockerfile_path: PathBuf,
    pub context_dir: PathBuf,
    pub build_args: HashMap<String, String>,
    pub tag: String,
}

/// Options for building Docker images
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub dockerfile_path: Option<PathBuf>,
    pub context_path: PathBuf,
    pub build_args: Vec<(String, String)>,
    pub no_cache: bool,
    pub target: Option<String>,
    pub labels: Vec<(String, String)>,
    pub pull: bool,
}

impl ImageBuilder {
    pub async fn new() -> Result<Self> {
        let docker =
            Docker::connect_with_local_defaults().context("Failed to connect to Docker")?;

        // Test connection
        docker.ping().await.context("Failed to ping Docker daemon")?;

        Ok(Self { docker })
    }

    /// Build a Docker image with the given options
    pub async fn build_image(
        &self,
        tag: &str,
        options: &BuildOptions,
        log_sender: Option<mpsc::Sender<String>>,
    ) -> Result<()> {
        info!("Building Docker image: {}", tag);

        // Create build context tar
        let build_context = self.create_build_context(options).await?;

        // Prepare build arguments
        let build_args: HashMap<&str, &str> =
            options.build_args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        // Prepare labels
        let mut labels = HashMap::new();
        for (key, value) in &options.labels {
            labels.insert(key.clone(), value.clone());
        }

        let build_image_options = BuildImageOptions {
            dockerfile: options
                .dockerfile_path
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("Dockerfile"),
            t: tag,
            pull: options.pull,
            nocache: options.no_cache,
            buildargs: build_args,
            labels: labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
            ..Default::default()
        };

        // Build the image
        let mut build_stream =
            self.docker.build_image(build_image_options, None, Some(build_context.into()));

        while let Some(build_result) = build_stream.next().await {
            match build_result {
                Ok(build_info) => {
                    if let Some(stream) = &build_info.stream {
                        debug!("Build: {}", stream.trim());
                        if let Some(ref sender) = log_sender {
                            let _ = sender.send(stream.clone()).await;
                        }
                    }
                    if let Some(error) = &build_info.error {
                        error!("Build error: {}", error);
                        return Err(anyhow::anyhow!("Build failed: {}", error));
                    }
                }
                Err(e) => {
                    error!("Build stream error: {}", e);
                    return Err(anyhow::anyhow!("Build stream error: {}", e));
                }
            }
        }

        info!("Successfully built image: {}", tag);
        Ok(())
    }

    /// Create build context tar from the given options
    async fn create_build_context(&self, options: &BuildOptions) -> Result<Vec<u8>> {
        let mut build_context = Vec::new();

        // Collect all files first
        let mut files = Vec::new();
        self.collect_files(&options.context_path, "", &mut files).await?;

        // Create tar archive
        let mut tar_builder = Builder::new(&mut build_context);

        for (tar_path, file_path, is_dir) in files {
            if is_dir {
                let mut header = Header::new_gnu();
                header.set_path(&format!("{}/", tar_path))?;
                header.set_size(0);
                header.set_mode(0o755);
                header.set_entry_type(tar::EntryType::Directory);
                header.set_cksum();
                tar_builder.append(&header, std::io::empty())?;
            } else {
                let mut file = tokio::fs::File::open(&file_path).await?;
                let metadata = file.metadata().await?;
                let mut header = Header::new_gnu();
                header.set_path(&tar_path)?;
                header.set_size(metadata.len());
                header.set_mode(0o644);
                header.set_cksum();

                let mut buffer = Vec::new();
                file.read_to_end(&mut buffer).await?;
                tar_builder.append(&header, std::io::Cursor::new(buffer))?;
            }
        }

        tar_builder.finish()?;
        drop(tar_builder);
        Ok(build_context)
    }

    /// Collect all files to add to tar
    fn collect_files<'a>(
        &'a self,
        dir_path: &'a Path,
        prefix: &'a str,
        files: &'a mut Vec<(String, PathBuf, bool)>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut entries = tokio::fs::read_dir(dir_path).await?;

            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let name = entry.file_name();
                let tar_path = if prefix.is_empty() {
                    name.to_string_lossy().to_string()
                } else {
                    format!("{}/{}", prefix, name.to_string_lossy())
                };

                if path.is_file() {
                    files.push((tar_path, path, false));
                } else if path.is_dir() {
                    // Skip common directories that shouldn't be in build context
                    if name == ".git"
                        || name == "node_modules"
                        || name == "target"
                        || name == ".cache"
                    {
                        continue;
                    }

                    files.push((tar_path.clone(), path.clone(), true));
                    self.collect_files(&path, &tar_path, files).await?;
                }
            }

            Ok(())
        })
    }

    /// Build an image from a container template
    pub async fn build_template(&self, template: &ContainerTemplate, tag: &str) -> Result<()> {
        self.build_template_with_logs(template, tag, None).await
    }

    /// Build an image from a container template with optional log sender
    pub async fn build_template_with_logs(
        &self,
        template: &ContainerTemplate,
        tag: &str,
        log_sender: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        match &template.config.image_source {
            ImageSource::Image { name } => {
                info!("Template uses pre-built image: {}", name);
                // Ensure image is available locally
                self.pull_image(name).await?;
                Ok(())
            }
            ImageSource::Dockerfile { path, build_args } => {
                info!("Building image from Dockerfile: {}", path.display());
                let context = BuildContext {
                    dockerfile_path: path.clone(),
                    context_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
                    build_args: build_args.clone(),
                    tag: tag.to_string(),
                };
                self.build_from_dockerfile_with_logs(&context, log_sender.as_ref()).await
            }
            ImageSource::ClaudeDocker {
                base_image,
                build_args,
            } => {
                info!("Building claude-docker based image");
                self.build_claude_dev_image_with_logs(
                    tag,
                    base_image.as_deref(),
                    build_args,
                    log_sender.as_ref(),
                )
                .await
            }
        }
    }

    /// Pull a pre-built image
    async fn pull_image(&self, image: &str) -> Result<()> {
        // Check if image exists locally first
        let images = self
            .docker
            .list_images(Some(bollard::image::ListImagesOptions::<String> {
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

        info!("Pulling image: {}", image);
        let options = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        debug!("Pull status: {}", status);
                    }
                }
                Err(e) => {
                    error!("Error pulling image: {}", e);
                    return Err(e.into());
                }
            }
        }

        info!("Successfully pulled image: {}", image);
        Ok(())
    }

    /// Build image from a standard Dockerfile
    async fn build_from_dockerfile(&self, context: &BuildContext) -> Result<()> {
        self.build_from_dockerfile_with_logs(context, None).await
    }

    /// Build image from a standard Dockerfile with optional log sender
    async fn build_from_dockerfile_with_logs(
        &self,
        context: &BuildContext,
        log_sender: Option<&mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        // Create build context tar
        let tar_data =
            self.create_build_context_sync(&context.context_dir, &context.dockerfile_path)?;

        let build_options = BuildImageOptions {
            dockerfile: "Dockerfile",
            t: context.tag.as_str(),
            buildargs: context.build_args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
            ..Default::default()
        };

        info!("Building image: {}", context.tag);
        let mut stream = self.docker.build_image(build_options, None, Some(tar_data.into()));

        while let Some(result) = stream.next().await {
            match result {
                Ok(output) => {
                    if let Some(stream) = output.stream {
                        if let Some(sender) = log_sender {
                            let _ = sender.send(stream.clone());
                        } else {
                            // Don't print to stdout when no log sender is provided
                            // This prevents disrupting the TUI
                            debug!("Docker build output: {}", stream.trim());
                        }
                    }
                    if let Some(error) = output.error {
                        let error_msg = format!("Build error: {}", error);
                        if let Some(sender) = log_sender {
                            let _ = sender.send(error_msg.clone());
                        }
                        error!("{}", error_msg);
                        return Err(anyhow::anyhow!("Docker build failed: {}", error));
                    }
                }
                Err(e) => {
                    error!("Build stream error: {}", e);
                    return Err(e.into());
                }
            }
        }

        info!("Successfully built image: {}", context.tag);
        Ok(())
    }

    /// Build claude-dev image based on our claude-docker setup
    async fn build_claude_dev_image(
        &self,
        tag: &str,
        base_image: Option<&str>,
        build_args: &HashMap<String, String>,
    ) -> Result<()> {
        self.build_claude_dev_image_with_logs(tag, base_image, build_args, None).await
    }

    /// Build claude-dev image based on our claude-docker setup with optional log sender
    async fn build_claude_dev_image_with_logs(
        &self,
        tag: &str,
        base_image: Option<&str>,
        build_args: &HashMap<String, String>,
        log_sender: Option<&mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        // Get the docker directory path
        let docker_dir = self.get_claude_dev_docker_dir()?;

        // Prepare build args
        let mut final_build_args = HashMap::new();

        // Set base image if specified
        if let Some(base) = base_image {
            final_build_args.insert("BASE_IMAGE".to_string(), base.to_string());
        }

        // Set host UID/GID for proper permissions
        if let Ok(uid) = std::env::var("UID") {
            final_build_args.insert("HOST_UID".to_string(), uid);
        } else {
            final_build_args.insert("HOST_UID".to_string(), "1000".to_string());
        }

        if let Ok(gid) = std::env::var("GID") {
            final_build_args.insert("HOST_GID".to_string(), gid);
        } else {
            final_build_args.insert("HOST_GID".to_string(), "1000".to_string());
        }

        // Add custom build args
        final_build_args.extend(build_args.clone());

        let context = BuildContext {
            dockerfile_path: docker_dir.join("Dockerfile"),
            context_dir: docker_dir,
            build_args: final_build_args,
            tag: tag.to_string(),
        };

        // Copy authentication files if they exist
        self.prepare_auth_files(&context.context_dir)?;

        self.build_from_dockerfile_with_logs(&context, log_sender).await
    }

    /// Get the claude-dev docker directory
    fn get_claude_dev_docker_dir(&self) -> Result<PathBuf> {
        // Try to find the docker directory relative to the current binary
        let current_exe =
            std::env::current_exe().context("Failed to get current executable path")?;

        let exe_dir = current_exe.parent().context("Failed to get executable directory")?;

        // Look for docker directory in a few possible locations
        let possible_paths = [
            exe_dir.join("../docker/agents-dev"),
            exe_dir.join("../../docker/agents-dev"),
            exe_dir.join("../../../docker/agents-dev"),
            PathBuf::from("./docker/agents-dev"),
            PathBuf::from("../docker/agents-dev"),
        ];

        for path in &possible_paths {
            if path.join("Dockerfile").exists() {
                return Ok(path.canonicalize()?);
            }
        }

        Err(anyhow::anyhow!(
            "Could not find agents-dev Dockerfile. Please ensure docker/agents-dev/Dockerfile exists"
        ))
    }

    /// Prepare authentication files for the build
    fn prepare_auth_files(&self, context_dir: &Path) -> Result<()> {
        // Copy .claude.json if it exists
        if let Some(home_dir) = dirs::home_dir() {
            let claude_auth = home_dir.join(".claude.json");
            if claude_auth.exists() {
                let dest = context_dir.join(".claude.json");
                std::fs::copy(&claude_auth, &dest).context("Failed to copy .claude.json")?;
                info!("Copied Claude authentication to build context");
            }
        }

        // Create .env file with available environment variables
        let env_vars = [
            "ANTHROPIC_API_KEY",
            "TWILIO_AUTH_TOKEN",
            "TWILIO_ACCOUNT_SID",
            "TWILIO_FROM_PHONE",
        ];

        let mut env_content = String::new();
        for var in &env_vars {
            if let Ok(value) = std::env::var(var) {
                env_content.push_str(&format!("{}={}\n", var, value));
            }
        }

        if !env_content.is_empty() {
            let env_file = context_dir.join(".env");
            std::fs::write(&env_file, env_content).context("Failed to write .env file")?;
            info!(
                "Created .env file with {} environment variables",
                env_vars.len()
            );
        }

        Ok(())
    }

    /// Create a tar archive for the build context (sync version)
    fn create_build_context_sync(&self, context_dir: &Path, dockerfile: &Path) -> Result<Vec<u8>> {
        let mut tar_data = Vec::new();
        let mut builder = Builder::new(&mut tar_data);

        // Add the Dockerfile
        let _dockerfile_name =
            dockerfile.file_name().unwrap_or_else(|| std::ffi::OsStr::new("Dockerfile"));

        let mut dockerfile_file = std::fs::File::open(dockerfile)?;
        let dockerfile_metadata = dockerfile_file.metadata()?;

        let mut header = Header::new_gnu();
        header.set_path("Dockerfile")?;
        header.set_size(dockerfile_metadata.len());
        header.set_mode(0o644);
        header.set_cksum();

        builder.append(&header, &mut dockerfile_file)?;

        // Add all files from context directory recursively
        self.add_directory_to_tar_sync(&mut builder, context_dir, "")?;

        builder.finish()?;
        drop(builder);

        Ok(tar_data)
    }

    /// Recursively add directory contents to tar
    fn add_directory_to_tar_sync(
        &self,
        builder: &mut Builder<&mut Vec<u8>>,
        dir: &Path,
        base_path: &str,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();

            // Skip hidden files and build artifacts
            if file_name_str.starts_with('.') || file_name_str == "Dockerfile" {
                continue;
            }

            let tar_path = if base_path.is_empty() {
                file_name_str.to_string()
            } else {
                format!("{}/{}", base_path, file_name_str)
            };

            if path.is_dir() {
                self.add_directory_to_tar_sync(builder, &path, &tar_path)?;
            } else {
                let mut file = std::fs::File::open(&path)?;
                let metadata = file.metadata()?;

                let mut header = Header::new_gnu();
                header.set_path(&tar_path)?;
                header.set_size(metadata.len());
                header.set_mode(0o644);
                header.set_cksum();

                builder.append(&header, &mut file)?;
            }
        }

        Ok(())
    }

    /// Check if an image exists locally
    pub async fn image_exists(&self, tag: &str) -> Result<bool> {
        let images = self
            .docker
            .list_images(Some(bollard::image::ListImagesOptions::<String> {
                filters: {
                    let mut filters = HashMap::new();
                    filters.insert("reference".to_string(), vec![tag.to_string()]);
                    filters
                },
                ..Default::default()
            }))
            .await?;

        Ok(!images.is_empty())
    }

    /// Remove an image
    pub async fn remove_image(&self, tag: &str) -> Result<()> {
        info!("Removing image: {}", tag);
        self.docker.remove_image(tag, None, None).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_image_builder_creation() {
        let builder = ImageBuilder::new().await;
        assert!(builder.is_ok());
    }

    #[tokio::test]
    async fn test_build_context_creation() {
        let temp_dir = TempDir::new().unwrap();
        let dockerfile = temp_dir.path().join("Dockerfile");
        std::fs::write(&dockerfile, "FROM alpine\nRUN echo hello").unwrap();

        let builder = ImageBuilder {
            docker: Docker::connect_with_local_defaults().unwrap(),
        };
        let build_options = BuildOptions {
            dockerfile_path: Some(dockerfile.clone()),
            context_path: temp_dir.path().to_path_buf(),
            build_args: vec![],
            no_cache: false,
            target: None,
            labels: vec![],
            pull: false,
        };
        let tar_data = builder.create_build_context(&build_options).await;
        assert!(tar_data.is_ok());
        assert!(!tar_data.unwrap().is_empty());
    }
}
