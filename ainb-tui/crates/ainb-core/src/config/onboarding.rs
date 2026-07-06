// ABOUTME: Onboarding configuration and persistence
// Tracks first-time setup completion and user preferences from the wizard

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Onboarding configuration persisted to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnboardingConfig {
    /// Whether onboarding has been completed
    #[serde(default)]
    pub completed: bool,

    /// When onboarding was completed (ISO 8601 timestamp)
    #[serde(default)]
    pub completed_at: Option<String>,

    /// Version of onboarding that was completed
    /// Used to trigger re-onboarding on major updates
    #[serde(default = "default_version")]
    pub version: String,

    /// Dependencies that user chose to skip
    #[serde(default)]
    pub skipped_dependencies: Vec<String>,

    /// Git directories configured during onboarding
    #[serde(default)]
    pub git_directories: Vec<PathBuf>,

    /// How the user found ainb (questionnaire answer)
    #[serde(default)]
    pub source: Option<String>,

    /// The user's role (questionnaire answer)
    #[serde(default)]
    pub role: Option<String>,

    /// What the user wants to do with ainb (questionnaire answer)
    #[serde(default)]
    pub use_case: Option<String>,
}

fn default_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

impl Default for OnboardingConfig {
    fn default() -> Self {
        Self {
            completed: false,
            completed_at: None,
            version: default_version(),
            skipped_dependencies: Vec::new(),
            git_directories: Vec::new(),
            source: None,
            role: None,
            use_case: None,
        }
    }
}

impl OnboardingConfig {
    /// Get the path to the onboarding config file
    pub fn config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home.join(".agents-in-a-box/config/onboarding.toml"))
    }

    /// Get the base agents-in-a-box directory
    pub fn base_dir() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home.join(".agents-in-a-box"))
    }

    /// Load onboarding config from disk
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path()?)
    }

    /// Load onboarding config from an explicit path.
    ///
    /// Returns the default config if the file does not exist. Used by the
    /// real loader (`load`) and by tests that need a hermetic path.
    pub fn load_from(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read onboarding config from {}", path.display()))?;

        let config: OnboardingConfig = toml::from_str(&content).with_context(|| {
            format!("Failed to parse onboarding config from {}", path.display())
        })?;

        Ok(config)
    }

    /// Save onboarding config to disk
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::config_path()?)
    }

    /// Save onboarding config to an explicit path, creating parent dirs.
    ///
    /// Backs the real saver (`save`) and lets tests persist to a temp dir
    /// without touching the user's home directory.
    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        // Ensure parent directories exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }

        let content =
            toml::to_string_pretty(self).context("Failed to serialize onboarding config")?;

        fs::write(path, content)
            .with_context(|| format!("Failed to write onboarding config to {}", path.display()))?;

        Ok(())
    }

    /// Mark onboarding as completed
    pub fn mark_completed(&mut self) {
        self.completed = true;
        self.completed_at = Some(Utc::now().to_rfc3339());
        self.version = default_version();
    }

    /// Check if onboarding needs to be run
    /// Returns true if:
    /// - Never completed
    /// - Major version changed (e.g., 1.x -> 2.x)
    pub fn needs_onboarding(&self) -> bool {
        if !self.completed {
            return true;
        }

        // Check for major version change
        let current_major = env!("CARGO_PKG_VERSION").split('.').next().unwrap_or("0");

        let saved_major = self.version.split('.').next().unwrap_or("0");

        current_major != saved_major
    }

    /// Reset onboarding state (factory reset)
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Perform full factory reset - removes all config files
    pub fn factory_reset() -> Result<()> {
        let base_dir = Self::base_dir()?;

        if base_dir.exists() {
            // Remove the entire .agents-in-a-box directory
            fs::remove_dir_all(&base_dir)
                .with_context(|| format!("Failed to remove {}", base_dir.display()))?;
        }

        Ok(())
    }

    /// Check if the base directory exists (first-time check)
    pub fn base_dir_exists() -> bool {
        if let Ok(base_dir) = Self::base_dir() {
            base_dir.exists()
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = OnboardingConfig::default();
        assert!(!config.completed);
        assert!(config.completed_at.is_none());
        assert!(config.skipped_dependencies.is_empty());
    }

    #[test]
    fn test_mark_completed() {
        let mut config = OnboardingConfig::default();
        assert!(!config.completed);

        config.mark_completed();

        assert!(config.completed);
        assert!(config.completed_at.is_some());
    }

    #[test]
    fn test_needs_onboarding_not_completed() {
        let config = OnboardingConfig::default();
        assert!(config.needs_onboarding());
    }

    #[test]
    fn test_needs_onboarding_completed() {
        let mut config = OnboardingConfig::default();
        config.mark_completed();
        assert!(!config.needs_onboarding());
    }

    #[test]
    fn test_reset() {
        let mut config = OnboardingConfig::default();
        config.mark_completed();
        config.skipped_dependencies.push("docker".to_string());

        config.reset();

        assert!(!config.completed);
        assert!(config.skipped_dependencies.is_empty());
    }

    #[test]
    fn test_questionnaire_answers_round_trip() {
        // USER-VISIBLE proof: the source/role/use-case answers the wizard
        // collects survive a save -> load cycle through the on-disk config.
        // No-op'ing the persist (dropping the field assignments below, or
        // the fields themselves) breaks the read-back assertions.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config/onboarding.toml");

        let mut config = OnboardingConfig::default();
        config.mark_completed();
        config.source = Some("Friend or colleague".to_string());
        config.role = Some("Software engineer".to_string());
        config.use_case = Some("Build features end-to-end".to_string());
        config.save_to(&path).unwrap();

        let loaded = OnboardingConfig::load_from(&path).unwrap();
        assert!(loaded.completed);
        assert_eq!(loaded.source.as_deref(), Some("Friend or colleague"));
        assert_eq!(loaded.role.as_deref(), Some("Software engineer"));
        assert_eq!(
            loaded.use_case.as_deref(),
            Some("Build features end-to-end")
        );

        // And confirm the answers are actually present in the persisted TOML
        // text, not just an in-memory artifact.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("source = \"Friend or colleague\""),
            "raw toml:\n{raw}"
        );
        assert!(
            raw.contains("role = \"Software engineer\""),
            "raw toml:\n{raw}"
        );
        assert!(
            raw.contains("use_case = \"Build features end-to-end\""),
            "raw toml:\n{raw}"
        );
    }

    #[test]
    fn test_questionnaire_defaults_are_none() {
        let config = OnboardingConfig::default();
        assert!(config.source.is_none());
        assert!(config.role.is_none());
        assert!(config.use_case.is_none());
    }
}
