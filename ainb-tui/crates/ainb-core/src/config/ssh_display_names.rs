// ABOUTME: Persistent storage for SSH session display names
// Stores custom display names in ~/.agents-in-a-box/ssh_display_names.json

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Store for SSH session display names
/// Maps tmux_session_name -> custom display_name
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SshDisplayNameStore {
    /// Map of tmux_session_name -> display_name
    #[serde(flatten)]
    names: HashMap<String, String>,
}

impl SshDisplayNameStore {
    /// Get the storage file path
    fn storage_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".agents-in-a-box").join("ssh_display_names.json"))
    }

    /// Load from disk (returns empty store if file doesn't exist)
    pub fn load() -> Self {
        Self::storage_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Save to disk
    pub fn save(&self) -> Result<(), std::io::Error> {
        if let Some(path) = Self::storage_path() {
            // Ensure directory exists
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let content = serde_json::to_string_pretty(self)?;
            fs::write(path, content)?;
        }
        Ok(())
    }

    /// Get display name for a tmux session
    pub fn get(&self, tmux_session_name: &str) -> Option<&String> {
        self.names.get(tmux_session_name)
    }

    /// Set display name (None removes it)
    pub fn set(&mut self, tmux_session_name: String, display_name: Option<String>) {
        match display_name {
            Some(name) => {
                self.names.insert(tmux_session_name, name);
            }
            None => {
                self.names.remove(&tmux_session_name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_store_get_set() {
        let mut store = SshDisplayNameStore::default();

        // Initially empty
        assert!(store.get("ssh-test-22").is_none());

        // Set a name
        store.set("ssh-test-22".to_string(), Some("My Server".to_string()));
        assert_eq!(store.get("ssh-test-22"), Some(&"My Server".to_string()));

        // Clear the name
        store.set("ssh-test-22".to_string(), None);
        assert!(store.get("ssh-test-22").is_none());
    }

    #[test]
    fn test_store_serialization() {
        let mut store = SshDisplayNameStore::default();
        store.set("ssh-prod-22".to_string(), Some("Production".to_string()));
        store.set("ssh-staging-22".to_string(), Some("Staging".to_string()));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: SshDisplayNameStore = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.get("ssh-prod-22"), Some(&"Production".to_string()));
        assert_eq!(loaded.get("ssh-staging-22"), Some(&"Staging".to_string()));
    }
}
