// ABOUTME: Unified session progress tracking for all container types
// Provides consistent progress reporting across different session creation paths

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Unified progress updates for session creation operations
/// This enum replaces the agents-dev specific progress enum and supports all container types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionProgress {
    // Configuration and validation phase
    LoadingConfiguration,
    ValidatingTemplate(String), // template name
    LoadingProjectConfig,

    // Workspace setup phase
    CreatingWorktree,
    InitializingWorkspace,

    // Authentication and environment phase
    SyncingAuthentication,
    CheckingEnvironment,
    ConfiguringGitHub,

    // Container preparation phase
    BuildingImage(String), // build log message
    PullingImage(String),  // image name
    PreparingContainer,

    // MCP server initialization phase
    InitializingMcpServers,
    InstallingMcpServer(String),  // server name
    ConfiguringMcpServer(String), // server name

    // Container lifecycle phase
    StartingContainer,
    WaitingForContainer,
    VerifyingContainer,

    // Final phase
    Ready,

    // Error states
    Error(String),
    Warning(String),
}

impl SessionProgress {
    /// Get a human-readable description of the progress step
    pub fn description(&self) -> String {
        match self {
            SessionProgress::LoadingConfiguration => "Loading configuration...".to_string(),
            SessionProgress::ValidatingTemplate(name) => {
                format!("Validating template '{}'...", name)
            }
            SessionProgress::LoadingProjectConfig => "Loading project configuration...".to_string(),
            SessionProgress::CreatingWorktree => "Creating worktree...".to_string(),
            SessionProgress::InitializingWorkspace => "Initializing workspace...".to_string(),
            SessionProgress::SyncingAuthentication => "Syncing authentication files...".to_string(),
            SessionProgress::CheckingEnvironment => "Checking environment...".to_string(),
            SessionProgress::ConfiguringGitHub => "Configuring GitHub...".to_string(),
            SessionProgress::BuildingImage(msg) => format!("Building image: {}", msg),
            SessionProgress::PullingImage(name) => format!("Pulling image '{}'...", name),
            SessionProgress::PreparingContainer => "Preparing container...".to_string(),
            SessionProgress::InitializingMcpServers => "Initializing MCP servers...".to_string(),
            SessionProgress::InstallingMcpServer(name) => {
                format!("Installing MCP server '{}'...", name)
            }
            SessionProgress::ConfiguringMcpServer(name) => {
                format!("Configuring MCP server '{}'...", name)
            }
            SessionProgress::StartingContainer => "Starting container...".to_string(),
            SessionProgress::WaitingForContainer => {
                "Waiting for container to be ready...".to_string()
            }
            SessionProgress::VerifyingContainer => "Verifying container status...".to_string(),
            SessionProgress::Ready => "Session ready!".to_string(),
            SessionProgress::Error(msg) => format!("Error: {}", msg),
            SessionProgress::Warning(msg) => format!("Warning: {}", msg),
        }
    }

    /// Check if this progress state indicates completion (success or failure)
    pub fn is_complete(&self) -> bool {
        matches!(self, SessionProgress::Ready | SessionProgress::Error(_))
    }

    /// Check if this progress state indicates an error
    pub fn is_error(&self) -> bool {
        matches!(self, SessionProgress::Error(_))
    }

    /// Check if this progress state indicates a warning
    pub fn is_warning(&self) -> bool {
        matches!(self, SessionProgress::Warning(_))
    }

    /// Get the phase of the session creation process
    pub fn phase(&self) -> SessionPhase {
        match self {
            SessionProgress::LoadingConfiguration
            | SessionProgress::ValidatingTemplate(_)
            | SessionProgress::LoadingProjectConfig => SessionPhase::Configuration,

            SessionProgress::CreatingWorktree | SessionProgress::InitializingWorkspace => {
                SessionPhase::Workspace
            }

            SessionProgress::SyncingAuthentication
            | SessionProgress::CheckingEnvironment
            | SessionProgress::ConfiguringGitHub => SessionPhase::Environment,

            SessionProgress::BuildingImage(_)
            | SessionProgress::PullingImage(_)
            | SessionProgress::PreparingContainer => SessionPhase::ContainerPrep,

            SessionProgress::InitializingMcpServers
            | SessionProgress::InstallingMcpServer(_)
            | SessionProgress::ConfiguringMcpServer(_) => SessionPhase::McpSetup,

            SessionProgress::StartingContainer
            | SessionProgress::WaitingForContainer
            | SessionProgress::VerifyingContainer => SessionPhase::ContainerLaunch,

            SessionProgress::Ready => SessionPhase::Complete,
            SessionProgress::Error(_) | SessionProgress::Warning(_) => SessionPhase::Error,
        }
    }
}

/// Phases of session creation for progress tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPhase {
    Configuration,
    Workspace,
    Environment,
    ContainerPrep,
    McpSetup,
    ContainerLaunch,
    Complete,
    Error,
}

impl SessionPhase {
    /// Get a human-readable name for the phase
    pub fn name(&self) -> &'static str {
        match self {
            SessionPhase::Configuration => "Configuration",
            SessionPhase::Workspace => "Workspace Setup",
            SessionPhase::Environment => "Environment Setup",
            SessionPhase::ContainerPrep => "Container Preparation",
            SessionPhase::McpSetup => "MCP Server Setup",
            SessionPhase::ContainerLaunch => "Container Launch",
            SessionPhase::Complete => "Complete",
            SessionPhase::Error => "Error",
        }
    }

    /// Get the estimated progress percentage for this phase (0-100)
    pub fn progress_percentage(&self) -> u8 {
        match self {
            SessionPhase::Configuration => 10,
            SessionPhase::Workspace => 20,
            SessionPhase::Environment => 35,
            SessionPhase::ContainerPrep => 50,
            SessionPhase::McpSetup => 70,
            SessionPhase::ContainerLaunch => 90,
            SessionPhase::Complete => 100,
            SessionPhase::Error => 0,
        }
    }
}

// Conversion from AgentsDevProgress for backward compatibility
impl From<crate::docker::agents_dev::AgentsDevProgress> for SessionProgress {
    fn from(agents_progress: crate::docker::agents_dev::AgentsDevProgress) -> Self {
        match agents_progress {
            crate::docker::agents_dev::AgentsDevProgress::SyncingAuthentication => {
                SessionProgress::SyncingAuthentication
            }
            crate::docker::agents_dev::AgentsDevProgress::CheckingEnvironment => {
                SessionProgress::CheckingEnvironment
            }
            crate::docker::agents_dev::AgentsDevProgress::BuildingImage(msg) => {
                SessionProgress::BuildingImage(msg)
            }
            crate::docker::agents_dev::AgentsDevProgress::StartingContainer => {
                SessionProgress::StartingContainer
            }
            crate::docker::agents_dev::AgentsDevProgress::ConfiguringGitHub => {
                SessionProgress::ConfiguringGitHub
            }
            crate::docker::agents_dev::AgentsDevProgress::Ready => SessionProgress::Ready,
            crate::docker::agents_dev::AgentsDevProgress::Error(msg) => SessionProgress::Error(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_descriptions() {
        let progress = SessionProgress::LoadingConfiguration;
        assert_eq!(progress.description(), "Loading configuration...");

        let progress = SessionProgress::BuildingImage("test".to_string());
        assert_eq!(progress.description(), "Building image: test");
    }

    #[test]
    fn test_progress_phases() {
        assert_eq!(
            SessionProgress::LoadingConfiguration.phase(),
            SessionPhase::Configuration
        );
        assert_eq!(
            SessionProgress::StartingContainer.phase(),
            SessionPhase::ContainerLaunch
        );
        assert_eq!(SessionProgress::Ready.phase(), SessionPhase::Complete);
    }

    #[test]
    fn test_completion_status() {
        assert!(SessionProgress::Ready.is_complete());
        assert!(SessionProgress::Error("test".to_string()).is_complete());
        assert!(!SessionProgress::LoadingConfiguration.is_complete());
    }

    #[test]
    fn test_phase_progress_percentages() {
        assert_eq!(SessionPhase::Configuration.progress_percentage(), 10);
        assert_eq!(SessionPhase::Complete.progress_percentage(), 100);
    }
}
