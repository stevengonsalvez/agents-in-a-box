// ABOUTME: Repository presets for per-repo configuration overrides
// Presets are stored in ~/.agents-in-a-box/presets/ and can be overridden
// per-repo in .agents-box/preset.toml

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A repository preset that defines default agent and configuration settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryPreset {
    /// Unique name for this preset
    pub name: String,

    /// Description of what this preset is for
    #[serde(default)]
    pub description: String,

    /// Agent provider (e.g., "claude", "codex", "gemini")
    #[serde(default = "default_provider")]
    pub agent_provider: String,

    /// Agent model (e.g., "opus", "sonnet", "haiku")
    #[serde(default = "default_model")]
    pub agent_model: String,

    /// Skills to enable for this preset
    #[serde(default)]
    pub skills: Vec<String>,

    /// Plugins to enable for this preset
    #[serde(default)]
    pub plugins: Vec<String>,

    /// Permission settings
    #[serde(default)]
    pub permissions: PermissionSet,

    /// Custom CLAUDE.md rules to append
    #[serde(default)]
    pub custom_rules: Option<String>,

    /// Environment variables to set
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

fn default_provider() -> String {
    "claude".to_string()
}

fn default_model() -> String {
    "sonnet".to_string()
}

/// Permission settings for a preset
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionSet {
    /// Allow file writes without confirmation
    #[serde(default)]
    pub file_write: bool,

    /// Allow shell commands without confirmation
    #[serde(default)]
    pub shell: bool,

    /// Allow git operations without confirmation
    #[serde(default)]
    pub git: bool,

    /// Allow network access without confirmation
    #[serde(default)]
    pub network: bool,

    /// Skip all permission prompts (dangerous)
    #[serde(default)]
    pub skip_all: bool,
}

impl Default for RepositoryPreset {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            description: "Default preset with balanced settings".to_string(),
            agent_provider: default_provider(),
            agent_model: default_model(),
            skills: Vec::new(),
            plugins: Vec::new(),
            permissions: PermissionSet::default(),
            custom_rules: None,
            environment: HashMap::new(),
        }
    }
}

/// Manager for repository presets
pub struct PresetManager {
    /// Directory where global presets are stored
    presets_dir: PathBuf,

    /// Cached presets
    presets: HashMap<String, RepositoryPreset>,
}

impl PresetManager {
    /// Create a new preset manager
    pub fn new() -> Result<Self> {
        let presets_dir = Self::presets_dir()?;

        // Ensure presets directory exists
        if !presets_dir.exists() {
            fs::create_dir_all(&presets_dir)
                .context("Failed to create presets directory")?;
        }

        let mut manager = Self {
            presets_dir,
            presets: HashMap::new(),
        };

        // Load existing presets
        manager.load_all()?;

        Ok(manager)
    }

    /// Get the presets directory path
    fn presets_dir() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .context("Failed to determine home directory")?;
        Ok(home.join(".agents-in-a-box").join("presets"))
    }

    /// Load all presets from the presets directory
    fn load_all(&mut self) -> Result<()> {
        if !self.presets_dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(&self.presets_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map(|e| e == "toml").unwrap_or(false) {
                if let Ok(preset) = self.load_preset(&path) {
                    self.presets.insert(preset.name.clone(), preset);
                }
            }
        }

        Ok(())
    }

    /// Load a single preset from a TOML file
    fn load_preset(&self, path: &Path) -> Result<RepositoryPreset> {
        let content = fs::read_to_string(path)
            .context(format!("Failed to read preset file: {:?}", path))?;

        let preset: RepositoryPreset = toml::from_str(&content)
            .context(format!("Failed to parse preset file: {:?}", path))?;

        Ok(preset)
    }

    /// Save a preset to the presets directory
    pub fn save_preset(&self, preset: &RepositoryPreset) -> Result<()> {
        let path = self.presets_dir.join(format!("{}.toml", preset.name));

        let content = toml::to_string_pretty(preset)
            .context("Failed to serialize preset")?;

        fs::write(&path, content)
            .context(format!("Failed to write preset file: {:?}", path))?;

        Ok(())
    }

    /// Get a preset by name
    pub fn get(&self, name: &str) -> Option<&RepositoryPreset> {
        self.presets.get(name)
    }

    /// Get all presets
    pub fn all(&self) -> Vec<&RepositoryPreset> {
        self.presets.values().collect()
    }

    /// List all preset names
    pub fn list_names(&self) -> Vec<&str> {
        self.presets.keys().map(|s| s.as_str()).collect()
    }

    /// Delete a preset
    pub fn delete(&mut self, name: &str) -> Result<()> {
        let path = self.presets_dir.join(format!("{}.toml", name));

        if path.exists() {
            fs::remove_file(&path)
                .context(format!("Failed to delete preset file: {:?}", path))?;
        }

        self.presets.remove(name);
        Ok(())
    }

    /// Load a repo-specific preset override if it exists
    pub fn load_repo_preset(repo_path: &Path) -> Result<Option<RepositoryPreset>> {
        let preset_path = repo_path.join(".agents-box").join("preset.toml");

        if !preset_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&preset_path)
            .context("Failed to read repo preset file")?;

        let preset: RepositoryPreset = toml::from_str(&content)
            .context("Failed to parse repo preset file")?;

        Ok(Some(preset))
    }
}

impl Default for PresetManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self {
            presets_dir: PathBuf::from("."),
            presets: HashMap::new(),
        })
    }
}

/// Create some built-in default presets
pub fn create_default_presets() -> Vec<RepositoryPreset> {
    vec![
        RepositoryPreset {
            name: "rust-backend".to_string(),
            description: "Rust backend development with testing and clippy".to_string(),
            agent_provider: "claude".to_string(),
            agent_model: "sonnet".to_string(),
            skills: vec!["test-writer-fixer".to_string(), "code-reviewer".to_string()],
            plugins: Vec::new(),
            permissions: PermissionSet {
                file_write: true,
                shell: true,
                git: true,
                network: false,
                skip_all: false,
            },
            custom_rules: Some("Always run `cargo clippy` before committing.".to_string()),
            environment: HashMap::new(),
        },
        RepositoryPreset {
            name: "typescript-frontend".to_string(),
            description: "TypeScript frontend with React and testing".to_string(),
            agent_provider: "claude".to_string(),
            agent_model: "sonnet".to_string(),
            skills: vec![
                "frontend-developer".to_string(),
                "tailwind-frontend-expert".to_string(),
            ],
            plugins: Vec::new(),
            permissions: PermissionSet {
                file_write: true,
                shell: true,
                git: true,
                network: true,
                skip_all: false,
            },
            custom_rules: Some("Use TypeScript strict mode. Prefer functional components.".to_string()),
            environment: HashMap::new(),
        },
        RepositoryPreset {
            name: "fast-iteration".to_string(),
            description: "Maximum speed - skip all prompts".to_string(),
            agent_provider: "claude".to_string(),
            agent_model: "haiku".to_string(),
            skills: Vec::new(),
            plugins: Vec::new(),
            permissions: PermissionSet {
                file_write: true,
                shell: true,
                git: true,
                network: true,
                skip_all: true,
            },
            custom_rules: None,
            environment: HashMap::new(),
        },
    ]
}
