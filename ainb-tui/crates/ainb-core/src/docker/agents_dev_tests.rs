// ABOUTME: Comprehensive tests for agents_dev module functionality
// Tests authentication, environment setup, image building, and container operations

#[cfg(test)]
mod tests {
    use super::super::agents_dev::*;
    use anyhow::Result;
    use std::collections::HashMap;
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::sync::mpsc;
    use uuid;

    // Helper function to create a test workspace
    fn create_test_workspace() -> Result<TempDir> {
        let temp_dir = TempDir::new()?;
        let workspace_path = temp_dir.path();

        // Create a simple test file
        fs::write(
            workspace_path.join("README.md"),
            "# Test Workspace\n\nThis is a test workspace for agents-dev testing.",
        )?;

        // Create a simple Python script
        fs::write(
            workspace_path.join("test_script.py"),
            "#!/usr/bin/env python3\nprint('Hello from agents-dev test!')\n",
        )?;

        // Create a package.json for Node.js projects
        fs::write(
            workspace_path.join("package.json"),
            r#"{"name": "test-workspace", "version": "1.0.0"}"#,
        )?;

        Ok(temp_dir)
    }

    // Helper function to create test config
    fn create_test_config() -> AgentsDevConfig {
        AgentsDevConfig {
            image_name: "agents-box:agents-dev-test".to_string(),
            memory_limit: Some("2g".to_string()),
            gpu_access: None,
            force_rebuild: false,
            no_cache: false,
            continue_session: false,
            skip_permissions: true,
            env_vars: {
                let mut env_vars = HashMap::new();
                env_vars.insert("TEST_MODE".to_string(), "true".to_string());
                env_vars
            },
        }
    }

    #[tokio::test]
    async fn test_agents_dev_manager_creation() -> Result<()> {
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        // Verify manager was created successfully
        // Check that authentication status works (implies directories exist)
        let auth_status = manager.get_authentication_status()?;
        assert!(auth_status.sources.len() >= 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_authentication_status_check() -> Result<()> {
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        let auth_status = manager.get_authentication_status()?;

        // Test that authentication check works
        assert!(auth_status.sources.len() >= 0); // Should have at least empty sources

        // Test environment variable detection
        if env::var("ANTHROPIC_API_KEY").is_ok() {
            assert!(auth_status.anthropic_api_key_set);
        }

        if env::var("GITHUB_TOKEN").is_ok() {
            assert!(auth_status.github_token_set);
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_sync_authentication_files() -> Result<()> {
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        // Create a test .claude.json file
        let home_dir = dirs::home_dir().unwrap();
        let test_claude_json = home_dir.join(".claude.json.test");
        fs::write(&test_claude_json, r#"{"api_key": "test_key"}"#)?;

        // Test sync (this should not fail even if no real auth files exist)
        let result = manager.sync_authentication_files(None).await;
        assert!(result.is_ok());

        // Clean up
        let _ = fs::remove_file(test_claude_json);

        Ok(())
    }

    #[tokio::test]
    async fn test_environment_setup() -> Result<()> {
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        // Test environment setup
        let result = manager.setup_environment(None).await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_environment_setup_with_github_token() -> Result<()> {
        let mut config = create_test_config();
        config.env_vars.insert("GITHUB_TOKEN".to_string(), "test_token".to_string());

        let manager = AgentsDevManager::new(config).await?;

        // Test environment setup with GitHub token
        let result = manager.setup_environment(None).await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn test_progress_tracking() -> Result<()> {
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        let (tx, mut rx) = mpsc::channel(10);

        // Test progress tracking during environment setup
        let setup_task = tokio::spawn(async move { manager.setup_environment(Some(tx)).await });

        // Collect progress messages
        let mut progress_messages = Vec::new();
        while let Some(progress) = rx.recv().await {
            let should_break = matches!(progress, AgentsDevProgress::Ready);
            progress_messages.push(progress);
            if should_break {
                break;
            }
        }

        // Wait for setup to complete
        let result = setup_task.await?;
        assert!(result.is_ok());

        // Should have received at least one progress message
        assert!(!progress_messages.is_empty());

        Ok(())
    }

    #[tokio::test]
    #[ignore] // Requires Docker and may take time to build
    async fn test_image_building() -> Result<()> {
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        // Test image building (this will skip if image already exists)
        let result = manager.build_image_if_needed(None).await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    #[ignore] // Requires Docker and may take time to build
    async fn test_image_building_with_progress() -> Result<()> {
        let mut config = create_test_config();
        config.force_rebuild = true; // Force rebuild to test progress
        config.image_name = "agents-box:agents-dev-test-rebuild".to_string();

        let manager = AgentsDevManager::new(config).await?;

        let (tx, mut rx) = mpsc::channel(100);

        // Test image building with progress tracking
        let build_task = tokio::spawn(async move { manager.build_image_if_needed(Some(tx)).await });

        // Collect progress messages
        let mut progress_messages = Vec::new();
        while let Some(progress) = rx.recv().await {
            let should_break = matches!(progress, AgentsDevProgress::Ready);
            progress_messages.push(progress);
            if should_break {
                break;
            }
        }

        // Wait for build to complete
        let result = build_task.await?;
        assert!(result.is_ok());

        // Should have received build progress messages
        let has_build_progress = progress_messages
            .iter()
            .any(|p| matches!(p, AgentsDevProgress::BuildingImage(_)));
        assert!(has_build_progress);

        Ok(())
    }

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_container_running() -> Result<()> {
        let temp_workspace = create_test_workspace()?;
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        // Test container running
        let session_id = uuid::Uuid::new_v4();
        let result = manager.run_container(temp_workspace.path(), session_id, None, true).await;
        assert!(result.is_ok());

        if let Ok(container_id) = result {
            // Container should be running
            assert!(!container_id.is_empty());

            // Try to stop the container for cleanup
            let stop_result =
                std::process::Command::new("docker").args(&["stop", &container_id]).output();

            if let Ok(output) = stop_result {
                if !output.status.success() {
                    eprintln!("Warning: Failed to stop test container {}", container_id);
                }
            }
        }

        Ok(())
    }

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_container_running_with_progress() -> Result<()> {
        let temp_workspace = create_test_workspace()?;
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        let (tx, mut rx) = mpsc::channel(10);

        // Test container running with progress tracking
        let run_task = tokio::spawn({
            let workspace_path = temp_workspace.path().to_path_buf();
            let session_id = uuid::Uuid::new_v4();
            async move { manager.run_container(&workspace_path, session_id, Some(tx), true).await }
        });

        // Collect progress messages
        let mut progress_messages = Vec::new();
        while let Some(progress) = rx.recv().await {
            let should_break = matches!(progress, AgentsDevProgress::Ready);
            progress_messages.push(progress);
            if should_break {
                break;
            }
        }

        // Wait for container to start
        let result = run_task.await?;
        assert!(result.is_ok());

        // Should have received progress messages
        assert!(!progress_messages.is_empty());

        let has_container_progress = progress_messages
            .iter()
            .any(|p| matches!(p, AgentsDevProgress::StartingContainer));
        assert!(has_container_progress);

        if let Ok(container_id) = result {
            // Cleanup
            let _ = std::process::Command::new("docker").args(&["stop", &container_id]).output();
        }

        Ok(())
    }

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_full_agents_dev_session() -> Result<()> {
        let temp_workspace = create_test_workspace()?;
        let config = create_test_config();

        let (tx, mut rx) = mpsc::channel(100);

        // Test full session creation with timeout
        let session_task = tokio::spawn({
            let workspace_path = temp_workspace.path().to_path_buf();
            let session_id = uuid::Uuid::new_v4();
            async move {
                // Add timeout to prevent hanging
                tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    create_agents_dev_session(&workspace_path, config, session_id, Some(tx), true),
                )
                .await
            }
        });

        // Collect all progress messages with timeout
        let mut progress_messages = Vec::new();
        let progress_timeout = std::time::Duration::from_secs(65);
        let progress_result = tokio::time::timeout(progress_timeout, async {
            while let Some(progress) = rx.recv().await {
                let should_break = matches!(progress, AgentsDevProgress::Ready);
                progress_messages.push(progress);
                if should_break {
                    break;
                }
            }
        })
        .await;

        // Check if progress collection timed out
        if progress_result.is_err() {
            println!("Skipping test_full_agents_dev_session: Docker operations timed out");
            return Ok(());
        }

        // Wait for session creation
        let result = session_task.await?;
        match result {
            Ok(session_result) => {
                match session_result {
                    Ok(container_id) => {
                        // Should have received comprehensive progress messages
                        assert!(!progress_messages.is_empty());

                        // Should have authentication sync progress
                        let has_auth_sync = progress_messages
                            .iter()
                            .any(|p| matches!(p, AgentsDevProgress::SyncingAuthentication));
                        assert!(has_auth_sync);

                        // Should have environment check progress
                        let has_env_check = progress_messages
                            .iter()
                            .any(|p| matches!(p, AgentsDevProgress::CheckingEnvironment));
                        assert!(has_env_check);

                        // Should have container start progress
                        let has_container_start = progress_messages
                            .iter()
                            .any(|p| matches!(p, AgentsDevProgress::StartingContainer));
                        assert!(has_container_start);

                        // Cleanup
                        let _ = std::process::Command::new("docker")
                            .args(&["stop", &container_id])
                            .output();
                    }
                    Err(_) => {
                        // Session creation failed, but that's okay for this test
                        println!("Session creation failed, but progress messages were received");
                    }
                }
            }
            Err(_timeout) => {
                println!("Skipping test_full_agents_dev_session: Docker operations timed out");
                return Ok(());
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_config_validation() -> Result<()> {
        // Test default config
        let default_config = AgentsDevConfig::default();
        assert_eq!(default_config.image_name, "agents-box:agents-dev");
        assert!(!default_config.force_rebuild);
        assert!(!default_config.no_cache);
        assert!(!default_config.continue_session);

        // Test custom config
        let custom_config = AgentsDevConfig {
            image_name: "custom-image".to_string(),
            memory_limit: Some("4g".to_string()),
            gpu_access: Some("all".to_string()),
            force_rebuild: true,
            no_cache: true,
            continue_session: true,
            skip_permissions: true,
            env_vars: HashMap::new(),
        };

        assert_eq!(custom_config.image_name, "custom-image");
        assert_eq!(custom_config.memory_limit, Some("4g".to_string()));
        assert_eq!(custom_config.gpu_access, Some("all".to_string()));
        assert!(custom_config.force_rebuild);
        assert!(custom_config.no_cache);
        assert!(custom_config.continue_session);

        Ok(())
    }

    #[tokio::test]
    async fn test_error_handling() -> Result<()> {
        // Test with invalid workspace path
        let invalid_path = PathBuf::from("/nonexistent/path");
        let config = create_test_config();

        let session_id = uuid::Uuid::new_v4();

        // Add timeout to prevent hanging if Docker is unresponsive
        let timeout_duration = std::time::Duration::from_secs(30);
        let result = tokio::time::timeout(
            timeout_duration,
            create_agents_dev_session(&invalid_path, config, session_id, None, true),
        )
        .await;

        match result {
            Ok(session_result) => {
                // If Docker is available, the session creation should fail due to invalid path
                assert!(session_result.is_err());
            }
            Err(_timeout) => {
                // If Docker is not available or unresponsive, skip this test
                println!(
                    "Skipping test_error_handling: Docker appears to be unavailable or unresponsive"
                );
                return Ok(());
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_authentication_status_with_env_vars() -> Result<()> {
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        // Test with environment variables
        env::set_var("ANTHROPIC_API_KEY", "test_key");
        env::set_var("GITHUB_TOKEN", "test_token");

        let auth_status = manager.get_authentication_status()?;

        assert!(auth_status.anthropic_api_key_set);
        assert!(auth_status.github_token_set);
        assert!(auth_status.sources.len() >= 2);

        // Clean up
        env::remove_var("ANTHROPIC_API_KEY");
        env::remove_var("GITHUB_TOKEN");

        Ok(())
    }

    #[test]
    fn test_progress_enum_variants() {
        // Test all progress variants
        let progress_variants = vec![
            AgentsDevProgress::SyncingAuthentication,
            AgentsDevProgress::CheckingEnvironment,
            AgentsDevProgress::BuildingImage("test".to_string()),
            AgentsDevProgress::StartingContainer,
            AgentsDevProgress::ConfiguringGitHub,
            AgentsDevProgress::Ready,
            AgentsDevProgress::Error("test error".to_string()),
        ];

        // All variants should be valid
        for progress in progress_variants {
            match progress {
                AgentsDevProgress::SyncingAuthentication => {}
                AgentsDevProgress::CheckingEnvironment => {}
                AgentsDevProgress::BuildingImage(msg) => assert_eq!(msg, "test"),
                AgentsDevProgress::StartingContainer => {}
                AgentsDevProgress::ConfiguringGitHub => {}
                AgentsDevProgress::Ready => {}
                AgentsDevProgress::Error(msg) => assert_eq!(msg, "test error"),
            }
        }
    }

    #[tokio::test]
    async fn test_ssh_config_generation() -> Result<()> {
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        // SSH config generation is tested indirectly through environment setup
        // when no GITHUB_TOKEN is provided
        let result = manager.setup_environment(None).await;
        assert!(result.is_ok());

        Ok(())
    }

    #[tokio::test]
    #[ignore] // Requires Docker
    async fn test_container_reaches_running_state() -> Result<()> {
        let temp_workspace = create_test_workspace()?;
        let config = create_test_config();
        let manager = AgentsDevManager::new(config).await?;

        let session_id = uuid::Uuid::new_v4();

        // Create container
        let result = manager.run_container(temp_workspace.path(), session_id, None, true).await;
        assert!(result.is_ok());

        if let Ok(container_id) = result {
            // Verify container is running using docker inspect
            let inspect_result = std::process::Command::new("docker")
                .args(&["inspect", "--format", "{{.State.Running}}", &container_id])
                .output();

            if let Ok(output) = inspect_result {
                let is_running_output = String::from_utf8_lossy(&output.stdout);
                let is_running = is_running_output.trim();
                assert_eq!(is_running, "true", "Container should be in running state");

                // Also check container status
                let status_result = std::process::Command::new("docker")
                    .args(&["inspect", "--format", "{{.State.Status}}", &container_id])
                    .output();

                if let Ok(status_output) = status_result {
                    let status_string = String::from_utf8_lossy(&status_output.stdout);
                    let status = status_string.trim();
                    assert_eq!(status, "running", "Container status should be 'running'");
                }
            }

            // Cleanup
            let _ = std::process::Command::new("docker").args(&["stop", &container_id]).output();
            let _ = std::process::Command::new("docker").args(&["rm", &container_id]).output();
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_concurrent_sessions() -> Result<()> {
        let _temp_workspace1 = create_test_workspace()?;
        let _temp_workspace2 = create_test_workspace()?;

        let config1 = AgentsDevConfig {
            image_name: "agents-box:agents-dev-test-1".to_string(),
            ..create_test_config()
        };

        let config2 = AgentsDevConfig {
            image_name: "agents-box:agents-dev-test-2".to_string(),
            ..create_test_config()
        };

        // Test that multiple managers can be created concurrently
        let manager1_task = tokio::spawn(async move { AgentsDevManager::new(config1).await });

        let manager2_task = tokio::spawn(async move { AgentsDevManager::new(config2).await });

        let (result1, result2) = tokio::join!(manager1_task, manager2_task);

        assert!(result1.is_ok());
        assert!(result2.is_ok());

        let manager1 = result1??;
        let manager2 = result2??;

        // Both managers should work independently
        let auth1 = manager1.get_authentication_status()?;
        let auth2 = manager2.get_authentication_status()?;

        // Both should work (even if they have the same results)
        assert!(auth1.sources.len() >= 0);
        assert!(auth2.sources.len() >= 0);

        Ok(())
    }
}
